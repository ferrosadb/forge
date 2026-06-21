use crate::extract::Edge;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// N×N Design Structure Matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsmMatrix {
    pub labels: Vec<String>,
    pub data: Vec<Vec<f64>>,
    #[serde(skip)]
    pub label_index: HashMap<String, usize>,
}

impl DsmMatrix {
    /// Build a binary DSM from edges (1.0 if any edge exists).
    pub fn from_edges(edges: &[Edge]) -> Self {
        let mut names = std::collections::BTreeSet::new();
        for e in edges {
            names.insert(e.source.clone());
            names.insert(e.target.clone());
        }
        let labels: Vec<String> = names.into_iter().collect();
        let mut label_index = HashMap::new();
        for (i, l) in labels.iter().enumerate() {
            label_index.insert(l.clone(), i);
        }
        let n = labels.len();
        let mut data = vec![vec![0.0; n]; n];
        for e in edges {
            if let (Some(&si), Some(&ti)) = (label_index.get(&e.source), label_index.get(&e.target))
            {
                if si != ti {
                    data[si][ti] = 1.0;
                }
            }
        }
        Self {
            labels,
            data,
            label_index,
        }
    }

    /// Build a numerical (weighted) DSM from edges, summing weights.
    pub fn from_edges_numerical(edges: &[Edge]) -> Self {
        let mut names = std::collections::BTreeSet::new();
        for e in edges {
            names.insert(e.source.clone());
            names.insert(e.target.clone());
        }
        let labels: Vec<String> = names.into_iter().collect();
        let mut label_index = HashMap::new();
        for (i, l) in labels.iter().enumerate() {
            label_index.insert(l.clone(), i);
        }
        let n = labels.len();
        let mut data = vec![vec![0.0; n]; n];
        for e in edges {
            if let (Some(&si), Some(&ti)) = (label_index.get(&e.source), label_index.get(&e.target))
            {
                if si != ti {
                    data[si][ti] += e.weight;
                }
            }
        }
        Self {
            labels,
            data,
            label_index,
        }
    }

    /// Filter to labels matching a prefix, keeping only edges within the filtered set.
    pub fn filter_prefix(&self, prefix: &str) -> Self {
        let indices: Vec<usize> = self
            .labels
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with(prefix))
            .map(|(i, _)| i)
            .collect();
        self.submatrix(&indices)
    }

    /// Aggregate labels to a given dot-separated depth.
    /// E.g., depth=3 collapses "a.b.c.d.e" to "a.b.c".
    pub fn aggregate(&self, depth: usize) -> Self {
        let truncated: Vec<String> = self
            .labels
            .iter()
            .map(|l| {
                let parts: Vec<&str> = l.split('.').collect();
                if parts.len() <= depth {
                    l.clone()
                } else {
                    parts[..depth].join(".")
                }
            })
            .collect();

        let mut unique_labels: Vec<String> = truncated
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        unique_labels.sort();
        let mut new_index = HashMap::new();
        for (i, l) in unique_labels.iter().enumerate() {
            new_index.insert(l.clone(), i);
        }

        let n = unique_labels.len();
        let mut data = vec![vec![0.0; n]; n];

        for (si, sl) in truncated.iter().enumerate() {
            for (ti, tl) in truncated.iter().enumerate() {
                if self.data[si][ti] > 0.0 {
                    let ni = new_index[sl];
                    let nti = new_index[tl];
                    if ni != nti {
                        data[ni][nti] += self.data[si][ti];
                    }
                }
            }
        }

        Self {
            labels: unique_labels,
            data,
            label_index: new_index,
        }
    }

    /// Transpose the matrix.
    pub fn transpose(&self) -> Self {
        let n = self.size();
        let mut data = vec![vec![0.0; n]; n];
        for (i, src_row) in self.data.iter().enumerate() {
            for (j, &val) in src_row.iter().enumerate() {
                data[j][i] = val;
            }
        }
        Self {
            labels: self.labels.clone(),
            data,
            label_index: self.label_index.clone(),
        }
    }

    /// Convert to binary (any non-zero → 1.0).
    pub fn to_binary(&self) -> Self {
        let data = self
            .data
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&v| if v > 0.0 { 1.0 } else { 0.0 })
                    .collect()
            })
            .collect();
        Self {
            labels: self.labels.clone(),
            data,
            label_index: self.label_index.clone(),
        }
    }

    /// Reorder rows and columns by given index permutation.
    pub fn reorder(&self, order: &[usize]) -> Self {
        let n = order.len();
        let labels: Vec<String> = order.iter().map(|&i| self.labels[i].clone()).collect();
        let mut label_index = HashMap::new();
        for (i, l) in labels.iter().enumerate() {
            label_index.insert(l.clone(), i);
        }
        let mut data = vec![vec![0.0; n]; n];
        for (ni, &oi) in order.iter().enumerate() {
            for (nj, &oj) in order.iter().enumerate() {
                data[ni][nj] = self.data[oi][oj];
            }
        }
        Self {
            labels,
            data,
            label_index,
        }
    }

    /// Extract a submatrix for the given indices.
    pub fn submatrix(&self, indices: &[usize]) -> Self {
        let n = indices.len();
        let labels: Vec<String> = indices.iter().map(|&i| self.labels[i].clone()).collect();
        let mut label_index = HashMap::new();
        for (i, l) in labels.iter().enumerate() {
            label_index.insert(l.clone(), i);
        }
        let mut data = vec![vec![0.0; n]; n];
        for (ni, &oi) in indices.iter().enumerate() {
            for (nj, &oj) in indices.iter().enumerate() {
                data[ni][nj] = self.data[oi][oj];
            }
        }
        Self {
            labels,
            data,
            label_index,
        }
    }

    /// Number of elements in the matrix.
    pub fn size(&self) -> usize {
        self.labels.len()
    }

    /// Rebuild the label_index from labels (e.g., after deserialization).
    pub fn rebuild_index(&mut self) {
        self.label_index.clear();
        for (i, l) in self.labels.iter().enumerate() {
            self.label_index.insert(l.clone(), i);
        }
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
    fn from_edges_builds_correct_matrix() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("a", "c"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        assert_eq!(m.size(), 3);
        let ai = m.label_index["a"];
        let bi = m.label_index["b"];
        let ci = m.label_index["c"];
        assert_eq!(m.data[ai][bi], 1.0);
        assert_eq!(m.data[bi][ci], 1.0);
        assert_eq!(m.data[ai][ci], 1.0);
        assert_eq!(m.data[ci][ai], 0.0);
    }

    #[test]
    fn numerical_sums_weights() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("a", "b"),
            make_edge("a", "b"),
        ];
        let m = DsmMatrix::from_edges_numerical(&edges);
        let ai = m.label_index["a"];
        let bi = m.label_index["b"];
        assert_eq!(m.data[ai][bi], 3.0);
    }

    #[test]
    fn filter_prefix_works() {
        let edges = vec![
            make_edge("com.a", "com.b"),
            make_edge("com.a", "org.c"),
            make_edge("org.c", "com.b"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let filtered = m.filter_prefix("com.");
        assert_eq!(filtered.size(), 2);
        assert!(filtered.label_index.contains_key("com.a"));
        assert!(filtered.label_index.contains_key("com.b"));
    }

    #[test]
    fn aggregate_collapses_depth() {
        let edges = vec![
            make_edge("a.b.c", "a.b.d"),
            make_edge("a.b.c", "a.x.y"),
            make_edge("a.x.y", "a.b.c"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let agg = m.aggregate(2);
        assert_eq!(agg.size(), 2); // a.b and a.x
        let ab = agg.label_index["a.b"];
        let ax = agg.label_index["a.x"];
        assert!(agg.data[ab][ax] > 0.0);
        assert!(agg.data[ax][ab] > 0.0);
        // internal a.b.c -> a.b.d collapses to self, not counted
        assert_eq!(agg.data[ab][ab], 0.0);
    }

    #[test]
    fn transpose_swaps() {
        let edges = vec![make_edge("a", "b")];
        let m = DsmMatrix::from_edges(&edges);
        let t = m.transpose();
        let ai = t.label_index["a"];
        let bi = t.label_index["b"];
        assert_eq!(t.data[bi][ai], 1.0);
        assert_eq!(t.data[ai][bi], 0.0);
    }

    #[test]
    fn reorder_permutes() {
        let edges = vec![make_edge("a", "b"), make_edge("b", "c")];
        let m = DsmMatrix::from_edges(&edges);
        let ai = m.label_index["a"];
        let bi = m.label_index["b"];
        let ci = m.label_index["c"];
        let reordered = m.reorder(&[ci, bi, ai]);
        assert_eq!(reordered.labels[0], "c");
        assert_eq!(reordered.labels[1], "b");
        assert_eq!(reordered.labels[2], "a");
    }

    #[test]
    fn submatrix_extracts() {
        let edges = vec![
            make_edge("a", "b"),
            make_edge("b", "c"),
            make_edge("a", "c"),
        ];
        let m = DsmMatrix::from_edges(&edges);
        let ai = m.label_index["a"];
        let ci = m.label_index["c"];
        let sub = m.submatrix(&[ai, ci]);
        assert_eq!(sub.size(), 2);
    }

    #[test]
    fn self_loops_excluded() {
        let edges = vec![make_edge("a", "a")];
        let m = DsmMatrix::from_edges(&edges);
        assert_eq!(m.data[0][0], 0.0);
    }
}
