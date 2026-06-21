use crate::cluster::ClusterResult;
use crate::cycles::CycleInfo;
use crate::matrix::DsmMatrix;
use serde::{Deserialize, Serialize};

/// Full DSM metrics report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsmMetrics {
    pub propagation_cost: f64,
    pub max_cycle_size: usize,
    pub num_cycles: usize,
    pub cluster_quality: f64,
    pub elements: Vec<ElementMetrics>,
    pub projected: Option<ProjectedMetrics>,
}

/// Projected metrics after proposed refactoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectedMetrics {
    pub propagation_cost: f64,
    pub max_cycle_size: usize,
    pub num_cycles: usize,
    pub cluster_quality: f64,
    pub improvement_summary: String,
}

/// Per-element metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementMetrics {
    pub name: String,
    pub fan_in: usize,
    pub fan_out: usize,
    pub instability: f64,
    pub in_cycle: bool,
    pub cycle_id: Option<usize>,
    pub cluster_id: usize,
    pub module: Option<String>,
}

/// Compute full metrics for a DSM.
pub fn compute_metrics(
    matrix: &DsmMatrix,
    cycles: &CycleInfo,
    clusters: &ClusterResult,
) -> DsmMetrics {
    let n = matrix.size();

    // Propagation cost: reachability / N^2
    let pc = compute_propagation_cost(matrix);

    // Build cycle membership map
    let mut cycle_of: Vec<Option<usize>> = vec![None; n];
    for (ci, cycle) in cycles.cycles.iter().enumerate() {
        for &elem in cycle {
            cycle_of[elem] = Some(ci);
        }
    }

    // Build cluster membership
    let mut cluster_of = vec![0usize; n];
    for c in &clusters.clusters {
        for &elem in &c.elements {
            cluster_of[elem] = c.id;
        }
    }

    // Per-element metrics
    let elements: Vec<ElementMetrics> = (0..n)
        .map(|i| {
            let fan_in = (0..n).filter(|&j| matrix.data[j][i] > 0.0).count();
            let fan_out = (0..n).filter(|&j| matrix.data[i][j] > 0.0).count();
            let instability = if fan_in + fan_out > 0 {
                fan_out as f64 / (fan_in + fan_out) as f64
            } else {
                0.0
            };
            ElementMetrics {
                name: matrix.labels[i].clone(),
                fan_in,
                fan_out,
                instability,
                in_cycle: cycle_of[i].is_some(),
                cycle_id: cycle_of[i],
                cluster_id: cluster_of[i],
                module: None,
            }
        })
        .collect();

    DsmMetrics {
        propagation_cost: pc,
        max_cycle_size: cycles.max_cycle_size,
        num_cycles: cycles.cycles.len(),
        cluster_quality: clusters.quality,
        elements,
        projected: None,
    }
}

/// Compute propagation cost using Warshall's transitive closure.
/// PC = (number of reachable pairs) / N^2
pub fn compute_propagation_cost(matrix: &DsmMatrix) -> f64 {
    let n = matrix.size();
    if n == 0 {
        return 0.0;
    }

    // Build reachability matrix using Warshall's algorithm
    let mut reach = vec![vec![false; n]; n];
    for (i, src_row) in matrix.data.iter().enumerate() {
        for (j, &val) in src_row.iter().enumerate() {
            if val > 0.0 {
                reach[i][j] = true;
            }
        }
        reach[i][i] = true; // self-reachable
    }

    for k in 0..n {
        for i in 0..n {
            for j in 0..n {
                if reach[i][k] && reach[k][j] {
                    reach[i][j] = true;
                }
            }
        }
    }

    let reachable_count: usize = reach
        .iter()
        .flat_map(|row| row.iter())
        .filter(|&&r| r)
        .count();

    reachable_count as f64 / (n * n) as f64
}

/// Categorize propagation cost.
pub fn categorize_pc(pc: f64) -> &'static str {
    if pc < 0.10 {
        "good"
    } else if pc < 0.30 {
        "warning"
    } else {
        "critical"
    }
}

/// Categorize max cycle size.
pub fn categorize_cycle_size(size: usize) -> &'static str {
    if size <= 2 {
        "good"
    } else if size <= 10 {
        "warning"
    } else {
        "critical"
    }
}

/// Categorize cluster quality.
pub fn categorize_quality(quality: f64) -> &'static str {
    if quality > 0.80 {
        "good"
    } else if quality > 0.50 {
        "warning"
    } else {
        "critical"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{cluster, ClusterConfig};
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
    fn pc_of_empty_is_zero() {
        let m = DsmMatrix::from_edges(&[]);
        assert_eq!(compute_propagation_cost(&m), 0.0);
    }

    #[test]
    fn pc_of_disconnected_is_low() {
        // 3 isolated nodes: only self-reachable = 3/9
        let edges = vec![
            make_edge("a", "b"), // need edges to create nodes
        ];
        let m = DsmMatrix::from_edges(&edges);
        let pc = compute_propagation_cost(&m);
        // a can reach b, a and b can reach themselves = 3/4 = 0.75
        assert!(pc > 0.5);
    }

    #[test]
    fn pc_of_chain() {
        // a -> b -> c: a reaches all, b reaches b+c, c reaches c
        // reachable: (a,a), (a,b), (a,c), (b,b), (b,c), (c,c) = 6/9
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let pc = compute_propagation_cost(&m);
        assert!((pc - 6.0 / 9.0).abs() < 0.01);
    }

    #[test]
    fn pc_of_full_cycle() {
        // a -> b -> c -> a: everyone reaches everyone = 9/9 = 1.0
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("c", "a"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let pc = compute_propagation_cost(&m);
        assert!((pc - 1.0).abs() < 0.01);
    }

    #[test]
    fn full_metrics_computed() {
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
        assert_eq!(metrics.elements.len(), 4);
        assert!(metrics.num_cycles > 0);
    }

    #[test]
    fn categorize_functions() {
        assert_eq!(categorize_pc(0.05), "good");
        assert_eq!(categorize_pc(0.20), "warning");
        assert_eq!(categorize_pc(0.50), "critical");
        assert_eq!(categorize_cycle_size(0), "good");
        assert_eq!(categorize_cycle_size(5), "warning");
        assert_eq!(categorize_cycle_size(15), "critical");
        assert_eq!(categorize_quality(0.9), "good");
        assert_eq!(categorize_quality(0.6), "warning");
        assert_eq!(categorize_quality(0.3), "critical");
    }
}
