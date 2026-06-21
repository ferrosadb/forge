use crate::matrix::DsmMatrix;
use serde::{Deserialize, Serialize};

/// Result of cycle detection via Tarjan's SCC algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleInfo {
    /// All strongly connected components (including singletons).
    pub components: Vec<Vec<usize>>,
    /// Only SCCs with size > 1 (actual cycles).
    pub cycles: Vec<Vec<usize>>,
    /// Size of the largest cycle.
    pub max_cycle_size: usize,
    /// Total number of elements participating in cycles.
    pub total_cycle_elements: usize,
    /// Edges that form part of cycles.
    pub cycle_edges: Vec<(usize, usize)>,
}

/// Find all strongly connected components using Tarjan's algorithm.
pub fn find_cycles(matrix: &DsmMatrix) -> CycleInfo {
    let n = matrix.size();
    let mut index_counter: usize = 0;
    let mut stack: Vec<usize> = Vec::new();
    let mut on_stack = vec![false; n];
    let mut indices = vec![usize::MAX; n];
    let mut lowlinks = vec![0usize; n];
    let mut components: Vec<Vec<usize>> = Vec::new();

    #[allow(clippy::too_many_arguments)]
    fn strongconnect(
        v: usize,
        matrix: &DsmMatrix,
        index_counter: &mut usize,
        stack: &mut Vec<usize>,
        on_stack: &mut Vec<bool>,
        indices: &mut Vec<usize>,
        lowlinks: &mut Vec<usize>,
        components: &mut Vec<Vec<usize>>,
    ) {
        indices[v] = *index_counter;
        lowlinks[v] = *index_counter;
        *index_counter += 1;
        stack.push(v);
        on_stack[v] = true;

        let n = matrix.size();
        for w in 0..n {
            if matrix.data[v][w] > 0.0 {
                if indices[w] == usize::MAX {
                    strongconnect(
                        w,
                        matrix,
                        index_counter,
                        stack,
                        on_stack,
                        indices,
                        lowlinks,
                        components,
                    );
                    lowlinks[v] = lowlinks[v].min(lowlinks[w]);
                } else if on_stack[w] {
                    lowlinks[v] = lowlinks[v].min(indices[w]);
                }
            }
        }

        if lowlinks[v] == indices[v] {
            let mut component = Vec::new();
            loop {
                let w = stack.pop().unwrap();
                on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            component.sort();
            components.push(component);
        }
    }

    for v in 0..n {
        if indices[v] == usize::MAX {
            strongconnect(
                v,
                matrix,
                &mut index_counter,
                &mut stack,
                &mut on_stack,
                &mut indices,
                &mut lowlinks,
                &mut components,
            );
        }
    }

    let cycles: Vec<Vec<usize>> = components.iter().filter(|c| c.len() > 1).cloned().collect();

    let max_cycle_size = cycles.iter().map(|c| c.len()).max().unwrap_or(0);
    let total_cycle_elements: usize = cycles.iter().map(|c| c.len()).sum();

    // Find edges that form part of cycles
    let mut cycle_edges = Vec::new();
    for cycle in &cycles {
        for &i in cycle {
            for &j in cycle {
                if i != j && matrix.data[i][j] > 0.0 {
                    cycle_edges.push((i, j));
                }
            }
        }
    }

    CycleInfo {
        components,
        cycles,
        max_cycle_size,
        total_cycle_elements,
        cycle_edges,
    }
}

/// Identify minimum edges to cut to break all cycles (tearing).
/// Uses a greedy heuristic: for each cycle, remove the edge with the
/// lowest weight (or highest fan-in target, breaking ties).
pub fn find_tear_edges(matrix: &DsmMatrix, cycles: &CycleInfo) -> Vec<(usize, usize, f64)> {
    let mut torn = Vec::new();
    let mut removed = std::collections::HashSet::new();

    // Build a mutable copy to track remaining edges
    let _n = matrix.size();
    let mut remaining = matrix.data.clone();

    for cycle in &cycles.cycles {
        // Check if this cycle is already broken
        if is_cycle_broken(cycle, &remaining) {
            continue;
        }

        // Find the weakest edge in this cycle to tear
        let mut best_edge: Option<(usize, usize, f64)> = None;
        for &i in cycle {
            for &j in cycle {
                if i != j && remaining[i][j] > 0.0 && !removed.contains(&(i, j)) {
                    let w = remaining[i][j];
                    if best_edge.is_none() || w < best_edge.unwrap().2 {
                        best_edge = Some((i, j, w));
                    }
                }
            }
        }

        if let Some((i, j, w)) = best_edge {
            remaining[i][j] = 0.0;
            removed.insert((i, j));
            torn.push((i, j, w));
        }
    }

    torn
}

/// Check if a cycle is broken in the given adjacency data.
fn is_cycle_broken(cycle: &[usize], data: &[Vec<f64>]) -> bool {
    // A cycle is broken if there's no path through all elements
    // Quick check: if any element has no outgoing edge to another cycle member
    for &i in cycle {
        let has_out = cycle.iter().any(|&j| i != j && data[i][j] > 0.0);
        if !has_out {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Edge, EdgeKind};
    use crate::matrix::DsmMatrix;

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
    fn no_cycles_in_dag() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        assert!(info.cycles.is_empty());
        assert_eq!(info.max_cycle_size, 0);
    }

    #[test]
    fn detects_simple_cycle() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "a")];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        assert_eq!(info.cycles.len(), 1);
        assert_eq!(info.max_cycle_size, 2);
        assert_eq!(info.total_cycle_elements, 2);
    }

    #[test]
    fn detects_three_node_cycle() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        assert_eq!(info.cycles.len(), 1);
        assert_eq!(info.max_cycle_size, 3);
    }

    #[test]
    fn detects_multiple_cycles() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "a"),
            make_edge("c", "d"),
            make_edge("d", "c"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        assert_eq!(info.cycles.len(), 2);
    }

    #[test]
    fn tear_edges_breaks_cycle() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        let tears = find_tear_edges(&m, &info);
        assert!(!tears.is_empty());
        // After removing tear edges, no cycles should remain
        let mut data = m.data.clone();
        for (i, j, _) in &tears {
            data[*i][*j] = 0.0;
        }
        // Verify the cycle is broken
        let labels = m.labels.clone();
        let mut new_edges: Vec<Edge> = Vec::new();
        for (i, row) in data.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                if v > 0.0 {
                    new_edges.push(make_edge(&labels[i], &labels[j]));
                }
            }
        }
        let m2 = DsmMatrix::from_edges(&new_edges);
        let info2 = find_cycles(&m2);
        assert!(info2.cycles.is_empty());
    }

    #[test]
    fn singletons_in_components() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let info = find_cycles(&m);
        // 3 singleton SCCs
        assert_eq!(info.components.len(), 3);
        assert!(info.cycles.is_empty());
    }
}
