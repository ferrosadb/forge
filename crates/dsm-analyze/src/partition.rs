use crate::cycles::{find_cycles, CycleInfo};
use crate::matrix::DsmMatrix;
use serde::{Deserialize, Serialize};

/// Result of partitioning (sequencing + banding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionResult {
    /// Topological order of elements.
    pub order: Vec<usize>,
    /// Bands: groups of elements that can be processed concurrently.
    pub layers: Vec<Vec<usize>>,
    /// Number of above-diagonal marks (feedback edges) after reordering.
    pub feedback_count: usize,
    /// Auto-generated layer names.
    pub layer_names: Vec<String>,
}

/// Partition the DSM using topological sort.
/// Cycles are collapsed into single nodes for ordering, then expanded.
pub fn partition(matrix: &DsmMatrix) -> PartitionResult {
    let n = matrix.size();
    let cycle_info = find_cycles(matrix);

    // Build SCC membership: element -> SCC id
    let mut scc_of = vec![0usize; n];
    for (scc_id, component) in cycle_info.components.iter().enumerate() {
        for &elem in component {
            scc_of[elem] = scc_id;
        }
    }

    let num_sccs = cycle_info.components.len();

    // Build condensed DAG (edges between SCCs)
    let mut dag = vec![std::collections::HashSet::new(); num_sccs];
    for i in 0..n {
        for j in 0..n {
            if matrix.data[i][j] > 0.0 && scc_of[i] != scc_of[j] {
                dag[scc_of[i]].insert(scc_of[j]);
            }
        }
    }

    // Topological sort on condensed DAG (Kahn's algorithm)
    let mut in_degree = vec![0usize; num_sccs];
    for neighbors in &dag {
        for &neighbor in neighbors {
            in_degree[neighbor] += 1;
        }
    }

    let mut queue: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut topo_order = Vec::new();
    while let Some(node) = queue.pop_front() {
        topo_order.push(node);
        for &neighbor in &dag[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push_back(neighbor);
            }
        }
    }

    // Reverse: Kahn puts sources first (dependents), but DSM wants
    // depended-upon elements first (foundation at top/left).
    topo_order.reverse();

    // If topo_order doesn't cover all SCCs (shouldn't happen after condensation),
    // append remaining
    if topo_order.len() < num_sccs {
        for i in 0..num_sccs {
            if !topo_order.contains(&i) {
                topo_order.push(i);
            }
        }
    }

    // Expand SCC order to element order
    let mut order = Vec::new();
    for &scc_id in &topo_order {
        let mut members = cycle_info.components[scc_id].clone();
        members.sort();
        order.extend(members);
    }

    // Compute bands (longest path layering on condensed DAG)
    let layers = compute_bands(matrix, &order, &cycle_info, &scc_of, &topo_order, &dag);

    // Count feedback edges (above-diagonal in reordered matrix).
    // In DSM convention, row i depends on col j. After topological sort,
    // dependencies should be below diagonal. Feedback = data[i][j] > 0 where i < j.
    let reordered = matrix.reorder(&order);
    let mut feedback_count = 0;
    for i in 0..n {
        for j in (i + 1)..n {
            if reordered.data[i][j] > 0.0 {
                feedback_count += 1;
            }
        }
    }

    let layer_names = generate_layer_names(layers.len());

    PartitionResult {
        order,
        layers,
        feedback_count,
        layer_names,
    }
}

/// Compute bands based on longest path in condensed DAG.
fn compute_bands(
    _matrix: &DsmMatrix,
    _order: &[usize],
    cycle_info: &CycleInfo,
    scc_of: &[usize],
    topo_order: &[usize],
    dag: &[std::collections::HashSet<usize>],
) -> Vec<Vec<usize>> {
    let num_sccs = cycle_info.components.len();

    // Compute longest path from each SCC
    let mut longest = vec![0usize; num_sccs];
    for &scc_id in topo_order.iter().rev() {
        for &neighbor in &dag[scc_id] {
            longest[scc_id] = longest[scc_id].max(longest[neighbor] + 1);
        }
    }

    // Max depth
    let max_depth = longest.iter().copied().max().unwrap_or(0);

    // Group elements by their band (inverted so foundation is band 0)
    let mut bands: Vec<Vec<usize>> = vec![Vec::new(); max_depth + 1];
    let n = scc_of.len();
    for elem in 0..n {
        let depth = max_depth - longest[scc_of[elem]];
        bands[depth].push(elem);
    }

    // Sort elements within each band
    for band in &mut bands {
        band.sort();
    }

    // Remove empty bands
    bands.retain(|b| !b.is_empty());

    bands
}

/// Generate human-readable layer names.
fn generate_layer_names(count: usize) -> Vec<String> {
    let base_names = [
        "Foundation",
        "Infrastructure",
        "Core",
        "Domain",
        "Business Logic",
        "Application",
        "Presentation",
        "Entry Points",
    ];
    (0..count)
        .map(|i| {
            if i < base_names.len() {
                base_names[i].to_string()
            } else {
                format!("Layer {}", i + 1)
            }
        })
        .collect()
}

/// Compute bands from a partition result.
pub fn band(matrix: &DsmMatrix, part: &PartitionResult) -> Vec<Vec<usize>> {
    // Re-use the layers from partition; this function exists for the API
    // but can also recompute with different parameters
    let _ = matrix;
    part.layers.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn partition_linear_chain() {
        // a -> b -> c should produce order [c, b, a] or equivalent
        // where dependencies point downward (earlier in order)
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let result = partition(&m);
        assert_eq!(result.order.len(), 3);
        // After reordering, no feedback edges
        assert_eq!(result.feedback_count, 0);
    }

    #[test]
    fn partition_with_cycle_has_feedback() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let result = partition(&m);
        // A 3-element cycle must have at least 1 feedback edge
        assert!(result.feedback_count >= 1);
    }

    #[test]
    fn partition_produces_bands() {
        let edges = vec![
            make_edge("app", "core"),
            make_edge("core", "util"),
            make_edge("app", "util"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let result = partition(&m);
        assert!(!result.layers.is_empty());
        // Should have at least 2 bands
        assert!(result.layers.len() >= 2);
    }

    #[test]
    fn layer_names_generated() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let result = partition(&m);
        assert_eq!(result.layer_names.len(), result.layers.len());
    }

    #[test]
    fn disconnected_graph() {
        let edges = vec![make_edge("a", "b"), make_edge("c", "d")];
        let m = DsmMatrix::from_edges(&edges);
        let result = partition(&m);
        assert_eq!(result.order.len(), 4);
        assert_eq!(result.feedback_count, 0);
    }
}
