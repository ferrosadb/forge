use crate::matrix::DsmMatrix;
use serde::{Deserialize, Serialize};

/// Configuration for clustering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub max_iterations: usize,
    pub num_runs: usize,
    pub seed: Option<u64>,
    pub pow_cc: f64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10_000,
            num_runs: 5,
            seed: None,
            pow_cc: 1.0,
        }
    }
}

/// Result of clustering analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterResult {
    pub clusters: Vec<Cluster>,
    pub cost: f64,
    pub quality: f64,
    pub inter_cluster_deps: Vec<InterClusterDep>,
}

/// A single cluster of related elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cluster {
    pub id: usize,
    pub name: Option<String>,
    pub elements: Vec<usize>,
    pub element_names: Vec<String>,
    pub internal_deps: usize,
    pub cohesion: f64,
}

/// A dependency between two clusters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterClusterDep {
    pub from_cluster: usize,
    pub to_cluster: usize,
    pub weight: f64,
    pub edges: Vec<(String, String)>,
}

/// Perform Thebeau clustering on the DSM.
///
/// The algorithm:
/// 1. Initialize each element in its own cluster
/// 2. Compute coordination cost
/// 3. Randomly propose element moves between clusters
/// 4. Accept moves that reduce cost
/// 5. Repeat for many iterations
/// 6. Run multiple times, keep best result
pub fn cluster(matrix: &DsmMatrix, config: &ClusterConfig) -> ClusterResult {
    use rand::prelude::*;

    let n = matrix.size();
    if n == 0 {
        return ClusterResult {
            clusters: vec![],
            cost: 0.0,
            quality: 1.0,
            inter_cluster_deps: vec![],
        };
    }

    let mut best_result: Option<(Vec<usize>, f64)> = None;
    let base_seed = config.seed.unwrap_or(42);

    for run in 0..config.num_runs {
        let mut rng = StdRng::seed_from_u64(base_seed.wrapping_add(run as u64));

        // Initialize: each element in its own cluster
        let mut assignment: Vec<usize> = (0..n).collect();
        let mut num_clusters = n;
        let mut current_cost = compute_cost(matrix, &assignment, num_clusters, config.pow_cc);

        for _iter in 0..config.max_iterations {
            // Pick a random element
            let elem = rng.random_range(0..n);
            // Pick a random target cluster (including a new one)
            let target_cluster = rng.random_range(0..num_clusters + 1);

            let old_cluster = assignment[elem];
            if target_cluster == old_cluster {
                continue;
            }

            // Try the move
            assignment[elem] = if target_cluster >= num_clusters {
                num_clusters += 1;
                num_clusters - 1
            } else {
                target_cluster
            };

            let new_cost = compute_cost(matrix, &assignment, num_clusters, config.pow_cc);

            if new_cost < current_cost {
                current_cost = new_cost;
                // Clean up empty clusters
                compact_clusters(&mut assignment, &mut num_clusters);
            } else {
                // Revert
                assignment[elem] = old_cluster;
                if target_cluster >= num_clusters {
                    num_clusters -= 1;
                }
            }
        }

        if best_result.is_none() || current_cost < best_result.as_ref().unwrap().1 {
            best_result = Some((assignment.clone(), current_cost));
        }
    }

    let (assignment, cost) = best_result.unwrap();
    build_cluster_result(matrix, &assignment, cost)
}

/// Compute coordination cost for the current clustering (Thebeau formula).
///
/// For each dependency DSM[i][j]:
/// - If i and j are in the same cluster of size s: cost += DSM[i][j] * s^pow_cc
/// - If i and j are in different clusters: cost += DSM[i][j] * n^pow_cc
///
/// This incentivizes grouping related elements: a cluster of size 3 costs 3^pow_cc
/// per internal dep, while leaving them unclustered costs n^pow_cc.
fn compute_cost(matrix: &DsmMatrix, assignment: &[usize], num_clusters: usize, pow_cc: f64) -> f64 {
    let n = matrix.size();
    let mut cost = 0.0;
    let n_penalty = (n as f64).powf(pow_cc);

    // Count cluster sizes
    let mut cluster_sizes = vec![0usize; num_clusters];
    for &c in assignment {
        if c < num_clusters {
            cluster_sizes[c] += 1;
        }
    }

    for i in 0..n {
        for j in 0..n {
            if matrix.data[i][j] > 0.0 {
                if assignment[i] == assignment[j] {
                    // Intra-cluster: cost proportional to cluster size
                    let s = cluster_sizes[assignment[i]] as f64;
                    cost += matrix.data[i][j] * s.powf(pow_cc);
                } else {
                    // Inter-cluster: penalize at full system size
                    cost += matrix.data[i][j] * n_penalty;
                }
            }
        }
    }

    cost
}

/// Remove empty clusters and renumber.
fn compact_clusters(assignment: &mut [usize], num_clusters: &mut usize) {
    let used: std::collections::BTreeSet<usize> = assignment.iter().copied().collect();
    if used.len() == *num_clusters {
        return;
    }
    let mapping: std::collections::HashMap<usize, usize> = used
        .iter()
        .enumerate()
        .map(|(new_id, &old_id)| (old_id, new_id))
        .collect();
    for a in assignment.iter_mut() {
        *a = mapping[a];
    }
    *num_clusters = used.len();
}

/// Build the final ClusterResult from assignment.
fn build_cluster_result(matrix: &DsmMatrix, assignment: &[usize], cost: f64) -> ClusterResult {
    let n = matrix.size();
    let num_clusters = *assignment.iter().max().unwrap_or(&0) + 1;

    let mut clusters = Vec::new();
    for c in 0..num_clusters {
        let elements: Vec<usize> = assignment
            .iter()
            .enumerate()
            .filter(|(_, &a)| a == c)
            .map(|(i, _)| i)
            .collect();
        let element_names: Vec<String> =
            elements.iter().map(|&i| matrix.labels[i].clone()).collect();

        // Count internal deps
        let mut internal_deps = 0usize;
        for &i in &elements {
            for &j in &elements {
                if i != j && matrix.data[i][j] > 0.0 {
                    internal_deps += 1;
                }
            }
        }

        let k = elements.len();
        let max_deps = if k > 1 { k * (k - 1) } else { 1 };
        let cohesion = internal_deps as f64 / max_deps as f64;

        clusters.push(Cluster {
            id: c,
            name: None,
            elements,
            element_names,
            internal_deps,
            cohesion,
        });
    }

    // Inter-cluster dependencies
    let mut inter_deps_map: std::collections::HashMap<(usize, usize), Vec<(String, String)>> =
        std::collections::HashMap::new();
    for i in 0..n {
        for j in 0..n {
            if matrix.data[i][j] > 0.0 && assignment[i] != assignment[j] {
                inter_deps_map
                    .entry((assignment[i], assignment[j]))
                    .or_default()
                    .push((matrix.labels[i].clone(), matrix.labels[j].clone()));
            }
        }
    }

    let inter_cluster_deps: Vec<InterClusterDep> = inter_deps_map
        .into_iter()
        .map(|((from, to), edges)| InterClusterDep {
            from_cluster: from,
            to_cluster: to,
            weight: edges.len() as f64,
            edges,
        })
        .collect();

    // Quality = ratio of intra-cluster deps to total deps
    let total_deps: usize = (0..n)
        .flat_map(|i| (0..n).map(move |j| (i, j)))
        .filter(|&(i, j)| matrix.data[i][j] > 0.0)
        .count();
    let intra_deps: usize = clusters.iter().map(|c| c.internal_deps).sum();
    let quality = if total_deps > 0 {
        intra_deps as f64 / total_deps as f64
    } else {
        1.0
    };

    ClusterResult {
        clusters,
        cost,
        quality,
        inter_cluster_deps,
    }
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
    fn cluster_empty_matrix() {
        let m = DsmMatrix::from_edges(&[]);
        let result = cluster(&m, &ClusterConfig::default());
        assert!(result.clusters.is_empty());
    }

    #[test]
    fn cluster_two_groups() {
        // Two strongly connected groups with weak inter-group link
        let edges = vec![
            make_edge("a1", "a2"),
            make_edge("a2", "a1"),
            make_edge("a1", "a3"),
            make_edge("a3", "a1"),
            make_edge("a2", "a3"),
            make_edge("a3", "a2"),
            make_edge("b1", "b2"),
            make_edge("b2", "b1"),
            make_edge("b1", "b3"),
            make_edge("b3", "b1"),
            make_edge("b2", "b3"),
            make_edge("b3", "b2"),
            make_edge("a1", "b1"), // single weak inter-group link
        ];
        let m = DsmMatrix::from_edges(&edges);
        let config = ClusterConfig {
            seed: Some(42),
            ..Default::default()
        };
        let result = cluster(&m, &config);
        // Should identify 2 or 3 clusters
        assert!(result.clusters.len() >= 2);
        // Stochastic algorithm: quality varies but should capture most internal deps
        assert!(result.quality > 0.3);
    }

    #[test]
    fn cluster_quality_bounded() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let result = cluster(
            &m,
            &ClusterConfig {
                seed: Some(1),
                ..Default::default()
            },
        );
        assert!(result.quality >= 0.0 && result.quality <= 1.0);
    }

    #[test]
    fn cluster_deterministic_with_seed() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "a"),
            make_edge("c", "d"),
            make_edge("d", "c"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let config = ClusterConfig {
            seed: Some(99),
            ..Default::default()
        };
        let r1 = cluster(&m, &config);
        let r2 = cluster(&m, &config);
        assert_eq!(r1.clusters.len(), r2.clusters.len());
    }
}
