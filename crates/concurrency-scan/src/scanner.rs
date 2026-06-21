//! Concurrency and distributed systems pattern scanner.
//!
//! Scans source files for consistency-relevant patterns: synchronization
//! primitives, consensus protocols, replication mechanisms, transaction
//! handling, and failure detection. Produces structured reports that
//! guide sub-agents to the right code in large database codebases.

use regex::Regex;
use serde::Serialize;

/// A category of concurrency/distributed systems pattern.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, Hash)]
pub enum Category {
    /// Locks, atomics, CAS, memory barriers, fences
    Synchronization,
    /// Paxos, Raft, quorum, ballot, leader election
    Consensus,
    /// Read-repair, anti-entropy, gossip, vector clocks, CRDTs
    Replication,
    /// 2PC, WAL, MVCC, snapshot, isolation levels
    Transaction,
    /// Timeout, partition, split-brain, fencing, lease
    Failure,
}

/// A tagged region within a source file.
#[derive(Debug, Serialize, PartialEq)]
pub struct Region {
    /// Enclosing function name, if detected
    pub function: Option<String>,
    /// First line of the region (1-indexed)
    pub line_start: usize,
    /// Last line of the region (1-indexed)
    pub line_end: usize,
    /// Categories matched in this region
    pub categories: Vec<Category>,
    /// Specific pattern keywords matched
    pub patterns_matched: Vec<String>,
    /// Human-readable composite label (e.g., "Paxos prepare phase")
    pub composite_label: Option<String>,
}

/// Scan results for a single file.
#[derive(Debug, Serialize, PartialEq)]
pub struct FileScan {
    pub path: String,
    pub regions: Vec<Region>,
}

/// Aggregate counts by category.
#[derive(Debug, Serialize, Default)]
pub struct CategorySummary {
    pub synchronization: usize,
    pub consensus: usize,
    pub replication: usize,
    pub transaction: usize,
    pub failure: usize,
}

/// Full scan report across all files.
#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub files: Vec<FileScan>,
    pub summary: CategorySummary,
}

/// Configuration for the scanner.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Which categories to scan for (empty = all)
    pub categories: Vec<Category>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            categories: vec![
                Category::Synchronization,
                Category::Consensus,
                Category::Replication,
                Category::Transaction,
                Category::Failure,
            ],
        }
    }
}

/// Scan a single source file for concurrency/distributed systems patterns.
pub fn scan(filename: &str, source: &str, config: &ScanConfig) -> FileScan {
    let lines: Vec<&str> = source.lines().collect();
    let functions = find_functions(&lines);
    let mut regions = Vec::new();

    for func in &functions {
        let end = func.end_line.min(lines.len());
        let func_source: Vec<&str> = lines[func.start_line - 1..end].to_vec();
        let matches = match_patterns(&func_source, config);

        if !matches.is_empty() {
            let mut categories: Vec<Category> = matches
                .iter()
                .flat_map(|m| m.categories.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            categories.sort_by_key(|c| format!("{c:?}"));

            let mut patterns: Vec<String> = matches
                .iter()
                .flat_map(|m| m.patterns.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            patterns.sort();

            let composite = derive_composite_label(&categories, &patterns);

            regions.push(Region {
                function: Some(func.name.clone()),
                line_start: func.start_line,
                line_end: end,
                categories,
                patterns_matched: patterns,
                composite_label: composite,
            });
        }
    }

    FileScan {
        path: filename.to_string(),
        regions,
    }
}

/// Build a full report from multiple file scans.
pub fn build_report(scans: Vec<FileScan>) -> ScanReport {
    let mut summary = CategorySummary::default();

    for file_scan in &scans {
        for region in &file_scan.regions {
            for cat in &region.categories {
                match cat {
                    Category::Synchronization => summary.synchronization += 1,
                    Category::Consensus => summary.consensus += 1,
                    Category::Replication => summary.replication += 1,
                    Category::Transaction => summary.transaction += 1,
                    Category::Failure => summary.failure += 1,
                }
            }
        }
    }

    ScanReport {
        files: scans
            .into_iter()
            .filter(|f| !f.regions.is_empty())
            .collect(),
        summary,
    }
}

// --- Private helpers ---

#[derive(Debug)]
struct FunctionInfo {
    name: String,
    start_line: usize, // 1-indexed
    end_line: usize,   // 1-indexed
}

struct PatternMatch {
    categories: Vec<Category>,
    patterns: Vec<String>,
}

fn find_functions(lines: &[&str]) -> Vec<FunctionInfo> {
    let func_re = Regex::new(
        r"(?x)
        (?:pub\s+)?(?:async\s+)?(?:fn|func|def|function)\s+(\w+)\s*\(
        | (?:defp?\s+)(\w+)\s*\(
        | (?:public|private|protected)\s+(?:static\s+)?(?:synchronized\s+)?(?:\w+\s+)(\w+)\s*\(
    ",
    )
    .unwrap();

    // Erlang: function_name(Args) -> or function_name(Args) when Guard ->
    let erlang_re = Regex::new(r"^([a-z_][a-z_0-9]*)\s*\(").unwrap();

    let mut functions = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(cap) = func_re.captures(lines[i]) {
            let name = cap
                .get(1)
                .or(cap.get(2))
                .or(cap.get(3))
                .map_or("unknown", |m| m.as_str())
                .to_string();
            let end_line = find_function_end(lines, i);
            let func = FunctionInfo {
                name,
                start_line: i + 1,
                end_line: end_line + 1,
            };
            i = end_line + 1;
            functions.push(func);
        } else if let Some(cap) = erlang_re.captures(lines[i]) {
            // Check this looks like an Erlang function head (has -> on this or next few lines)
            let name = cap.get(1).unwrap().as_str().to_string();
            if is_erlang_function_head(lines, i) {
                let end_line = find_erlang_function_end(lines, i);
                let func = FunctionInfo {
                    name,
                    start_line: i + 1,
                    end_line: end_line + 1,
                };
                i = end_line + 1;
                functions.push(func);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    functions
}

/// Check if a line starting with `name(` is an Erlang function head.
/// Looks for `->` on the same line or within the next few lines.
fn is_erlang_function_head(lines: &[&str], start: usize) -> bool {
    let look_ahead = 5.min(lines.len() - start);
    for line in &lines[start..start + look_ahead] {
        if line.contains("->") {
            return true;
        }
        // If we hit a line that's clearly not part of a function head, stop
        let trimmed = line.trim();
        if trimmed.ends_with('.') || trimmed.ends_with(';') {
            break;
        }
    }
    false
}

/// Find the end of an Erlang function. Functions end with a period `.`
/// as the last non-whitespace character. Multiple clauses are separated
/// by `;` and share the same function name.
fn find_erlang_function_end(lines: &[&str], start: usize) -> usize {
    let erlang_re = Regex::new(r"^([a-z_][a-z_0-9]*)\s*\(").unwrap();
    let func_name = erlang_re
        .captures(lines[start])
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or("");

    for (i, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim();
        if trimmed.ends_with('.') {
            return i;
        }
        // If we see a new function head (different name), the previous function ended
        if i > start {
            if let Some(cap) = erlang_re.captures(line) {
                let name = cap.get(1).unwrap().as_str();
                if name != func_name && is_erlang_function_head(lines, i) {
                    return i.saturating_sub(1);
                }
            }
        }
    }
    lines.len().saturating_sub(1)
}

fn find_function_end(lines: &[&str], start: usize) -> usize {
    let mut brace_depth = 0;
    let mut found_open = false;

    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
                if found_open && brace_depth == 0 {
                    return i;
                }
            }
        }
    }

    // Indentation-based fallback (Python, Elixir)
    if !found_open && start < lines.len() {
        let base_indent = lines[start].len() - lines[start].trim_start().len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let indent = line.len() - trimmed.len();
            if indent <= base_indent {
                return i.saturating_sub(1);
            }
        }
        // If we reach end of file, the function extends to the last non-empty line
        return lines.len().saturating_sub(1);
    }

    lines.len().saturating_sub(1)
}

fn is_in_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("--")
        || trimmed.starts_with('%')
        || trimmed.starts_with('*')
        || trimmed.starts_with("/*")
}

fn match_patterns(lines: &[&str], config: &ScanConfig) -> Vec<PatternMatch> {
    let mut found_categories = std::collections::HashSet::new();
    let mut found_patterns = Vec::new();

    for line in lines.iter() {
        if is_in_comment(line) {
            continue;
        }
        let lower = line.to_lowercase();

        for cat in &config.categories {
            let patterns = patterns_for_category(cat);
            for pattern in patterns {
                if lower.contains(pattern) {
                    found_categories.insert(cat.clone());
                    let pat_str = pattern.to_string();
                    if !found_patterns.contains(&pat_str) {
                        found_patterns.push(pat_str);
                    }
                }
            }
        }
    }

    if found_categories.is_empty() {
        return Vec::new();
    }

    vec![PatternMatch {
        categories: found_categories.into_iter().collect(),
        patterns: found_patterns,
    }]
}

fn patterns_for_category(category: &Category) -> &'static [&'static str] {
    match category {
        Category::Synchronization => &[
            "mutex",
            "rwlock",
            "read_lock",
            "write_lock",
            "lock_guard",
            "atomic",
            "compare_and_swap",
            "compare_and_set",
            "cas(",
            "memory_barrier",
            "fence",
            "volatile",
            "synchronized",
            "semaphore",
            "condvar",
            "condition_variable",
            "spin_lock",
            "spinlock",
            "read_write_lock",
            // C# / .NET
            "monitor.enter",
            "monitor.exit",
            "monitor.tryenter",
            "semaphoreslim",
            "interlocked.",
            "readerwriterlockslim",
            "readerwriterlock",
            "manualresetevent",
            "autoresetevent",
            "countdownevent",
            "barrier",
            "lock (",
            "lock(",
        ],
        Category::Consensus => &[
            "quorum",
            "ballot",
            "proposal",
            "proposer",
            "acceptor",
            "prepare",
            "promise",
            "accept",
            "paxos",
            "raft",
            "append_entries",
            "request_vote",
            "heartbeat",
            "leader_election",
            "leader_elect",
            "elect_leader",
            "term",
            "epoch",
            "log_entry",
            "commit_index",
            "viewstamp",
            "view_change",
        ],
        Category::Replication => &[
            "read_repair",
            "anti_entropy",
            "antientropy",
            "merkle_tree",
            "merkle_hash",
            "hash_tree",
            "hinted_handoff",
            "hinted_hand",
            "gossip",
            "gossip_protocol",
            "vector_clock",
            "vclock",
            "version_vector",
            "lamport_timestamp",
            "lamport_clock",
            "hlc",
            "hybrid_logical_clock",
            "crdt",
            "conflict_free",
            "lww_register",
            "last_write_wins",
            "conflict_resolution",
            "replication_factor",
            "replica_set",
        ],
        Category::Transaction => &[
            "two_phase_commit",
            "2pc",
            "prepare_commit",
            "write_ahead_log",
            "wal",
            "redo_log",
            "undo_log",
            "mvcc",
            "multi_version",
            "multiversion",
            "snapshot_isolation",
            "snapshot_read",
            "read_snapshot",
            "isolation_level",
            "read_committed",
            "serializable",
            "write_skew",
            "phantom_read",
            "dirty_read",
            "begin_transaction",
            "commit_transaction",
            "rollback",
            "transaction_log",
            "txn_log",
            // C# / .NET
            "transactionscope",
            "committabletransaction",
            "begintransaction",
        ],
        Category::Failure => &[
            "timeout",
            "connection_timeout",
            "request_timeout",
            "partition",
            "network_partition",
            "netsplit",
            "split_brain",
            "split_brain_detection",
            "fencing_token",
            "fencing",
            "fence_token",
            "lease",
            "lease_expiry",
            "lease_renewal",
            "unreachable",
            "node_down",
            "node_failure",
            "reconnect",
            "retry_policy",
            "backoff",
            "circuit_breaker",
            "failover",
            "failback",
        ],
    }
}

fn derive_composite_label(_categories: &[Category], patterns: &[String]) -> Option<String> {
    let pattern_set: std::collections::HashSet<&str> =
        patterns.iter().map(|s| s.as_str()).collect();

    // Paxos indicators
    if (pattern_set.contains("prepare") && pattern_set.contains("promise"))
        || (pattern_set.contains("ballot") && pattern_set.contains("acceptor"))
        || pattern_set.contains("paxos")
    {
        return Some("Paxos consensus".to_string());
    }

    // Raft indicators
    if pattern_set.contains("append_entries")
        || pattern_set.contains("request_vote")
        || (pattern_set.contains("raft") && pattern_set.contains("heartbeat"))
    {
        return Some("Raft consensus".to_string());
    }

    // Quorum read/write
    if pattern_set.contains("quorum")
        && (pattern_set.contains("read_repair") || pattern_set.contains("replication_factor"))
    {
        return Some("Quorum replication".to_string());
    }

    // MVCC transaction processing
    if pattern_set.contains("mvcc")
        || (pattern_set.contains("snapshot_isolation") && pattern_set.contains("write_skew"))
    {
        return Some("MVCC transaction processing".to_string());
    }

    // WAL / write-ahead logging
    if pattern_set.contains("wal") || pattern_set.contains("write_ahead_log") {
        return Some("Write-ahead logging".to_string());
    }

    // Two-phase commit
    if pattern_set.contains("2pc") || pattern_set.contains("two_phase_commit") {
        return Some("Two-phase commit".to_string());
    }

    // Leader election
    if pattern_set.contains("leader_election") || pattern_set.contains("elect_leader") {
        return Some("Leader election".to_string());
    }

    // Anti-entropy / read repair
    if pattern_set.contains("anti_entropy") || pattern_set.contains("read_repair") {
        return Some("Anti-entropy repair".to_string());
    }

    // Gossip protocol
    if pattern_set.contains("gossip") {
        return Some("Gossip protocol".to_string());
    }

    // Vector clocks / causal tracking
    if pattern_set.contains("vector_clock") || pattern_set.contains("vclock") {
        return Some("Causal ordering (vector clocks)".to_string());
    }

    // Failure handling
    if pattern_set.contains("split_brain") || pattern_set.contains("fencing_token") {
        return Some("Split-brain protection".to_string());
    }

    if pattern_set.contains("circuit_breaker") || pattern_set.contains("failover") {
        return Some("Failure recovery".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file() {
        let result = scan("test.rs", "", &ScanConfig::default());
        assert!(result.regions.is_empty());
    }

    #[test]
    fn detects_mutex_synchronization() {
        let source = r#"
fn acquire_lock(store: &Store) {
    let guard = store.mutex.lock().unwrap();
    let atomic_val = counter.compare_and_swap(old, new, Ordering::SeqCst);
    std::sync::atomic::fence(Ordering::Release);
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Synchronization));
        assert_eq!(region.function.as_deref(), Some("acquire_lock"));
    }

    #[test]
    fn detects_paxos_consensus() {
        let source = r#"
fn prepare_phase(ballot: u64, acceptors: &[Node]) -> Vec<Promise> {
    let proposal = Proposal::new(ballot);
    for acceptor in acceptors {
        acceptor.send_prepare(proposal.clone());
    }
    collect_promises(acceptors)
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Consensus));
        assert_eq!(region.composite_label.as_deref(), Some("Paxos consensus"));
    }

    #[test]
    fn detects_raft_consensus() {
        let source = r#"
fn handle_append_entries(entries: &[LogEntry]) -> bool {
    if self.stale() {
        return false;
    }
    self.reset_heartbeat_timer();
    self.append_to_log(entries);
    true
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Consensus));
        assert_eq!(region.composite_label.as_deref(), Some("Raft consensus"));
    }

    #[test]
    fn detects_vector_clock_replication() {
        let source = r#"
fn merge_replicas(local: &VectorClock, remote: &VectorClock) -> VectorClock {
    let merged = local.merge(remote);
    if merged.conflicts() {
        conflict_resolution(local, remote, LastWriteWins)
    } else {
        merged
    }
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Replication));
    }

    #[test]
    fn detects_mvcc_transaction() {
        let source = r#"
fn begin_snapshot_read(txn_id: u64) -> Snapshot {
    let snapshot = mvcc_store.create_snapshot(txn_id);
    snapshot.set_isolation_level(SnapshotIsolation);
    snapshot
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Transaction));
        assert_eq!(
            region.composite_label.as_deref(),
            Some("MVCC transaction processing")
        );
    }

    #[test]
    fn detects_split_brain_failure() {
        let source = r#"
fn handle_partition(cluster: &Cluster) -> Result<()> {
    if cluster.detect_split_brain() {
        let token = cluster.acquire_fencing_token()?;
        cluster.fence_minority_partition(token)?;
    }
    Ok(())
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let region = &result.regions[0];
        assert!(region.categories.contains(&Category::Failure));
        assert_eq!(
            region.composite_label.as_deref(),
            Some("Split-brain protection")
        );
    }

    #[test]
    fn skips_comments() {
        let source = r#"
fn normal_function() {
    // This mentions mutex and paxos but it's a comment
    let x = 42;
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert!(result.regions.is_empty());
    }

    #[test]
    fn handles_python_functions() {
        let source = r#"
def acquire_consensus(ballot_num, acceptors):
    proposal = Proposal(ballot_num)
    promises = send_prepare(proposal, acceptors)
    return promises
"#;
        let result = scan("test.py", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        assert!(result.regions[0].categories.contains(&Category::Consensus));
    }

    #[test]
    fn handles_java_functions() {
        let source = r#"
public synchronized void acquireLock(String resource) {
    ReentrantLock mutex = locks.get(resource);
    mutex.lock();
    try {
        compareAndSwap(resource, expected, desired);
    } finally {
        mutex.unlock();
    }
}
"#;
        let result = scan("Test.java", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        assert!(result.regions[0]
            .categories
            .contains(&Category::Synchronization));
    }

    #[test]
    fn handles_erlang_functions() {
        let source = r#"
merge(RObj) ->
    {Values, _Siblings} = merge_object(RObj),
    Values.

update(RObj, Actor, Operation) ->
    {Values0, Siblings} = merge_object(RObj),
    Values = apply_op(Values0, Actor, Operation),
    update_object(RObj, Values, Siblings).
"#;
        let result = scan("riak_kv_crdt.erl", source, &ScanConfig::default());
        // Neither function has concurrency patterns, so no regions
        assert!(result.regions.is_empty());
    }

    #[test]
    fn erlang_function_with_patterns() {
        let source = r#"
handle_handoff(Partition, Node) ->
    case riak_core_gossip:legacy_gossip() of
        true ->
            hinted_handoff(Partition, Node);
        false ->
            anti_entropy(Partition, Node)
    end.

do_read_repair(Key, Value, VClock) ->
    vector_clock:merge(VClock, Value),
    ok.
"#;
        let result = scan("riak_kv_vnode.erl", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 2);
        assert!(result.regions[0]
            .categories
            .contains(&Category::Replication));
        assert_eq!(
            result.regions[0].function.as_deref(),
            Some("handle_handoff")
        );
        assert!(result.regions[1]
            .categories
            .contains(&Category::Replication));
        assert_eq!(
            result.regions[1].function.as_deref(),
            Some("do_read_repair")
        );
    }

    #[test]
    fn erlang_multiclause_function() {
        let source = r#"
get_quorum(N) when N < 3 ->
    N;
get_quorum(N) ->
    N div 2 + 1.
"#;
        let result = scan("riak_kv_quorum.erl", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        assert!(result.regions[0].categories.contains(&Category::Consensus));
    }

    #[test]
    fn build_report_aggregates_counts() {
        let scan1 = FileScan {
            path: "a.rs".to_string(),
            regions: vec![Region {
                function: Some("f".to_string()),
                line_start: 1,
                line_end: 10,
                categories: vec![Category::Consensus, Category::Synchronization],
                patterns_matched: vec!["quorum".to_string()],
                composite_label: None,
            }],
        };
        let scan2 = FileScan {
            path: "b.rs".to_string(),
            regions: vec![],
        };
        let report = build_report(vec![scan1, scan2]);
        assert_eq!(report.summary.consensus, 1);
        assert_eq!(report.summary.synchronization, 1);
        assert_eq!(report.files.len(), 1); // empty file filtered out
    }

    #[test]
    fn multiple_categories_in_one_function() {
        let source = r#"
fn quorum_write(key: &str, value: &[u8]) -> Result<()> {
    let lock = self.mutex.lock()?;
    let wal_entry = write_ahead_log.append(key, value)?;
    let quorum = self.replication_factor / 2 + 1;
    for replica in &self.replicas[..quorum] {
        replica.send_with_timeout(wal_entry.clone())?;
    }
    Ok(())
}
"#;
        let result = scan("test.rs", source, &ScanConfig::default());
        assert_eq!(result.regions.len(), 1);
        let cats = &result.regions[0].categories;
        assert!(cats.contains(&Category::Synchronization)); // mutex
        assert!(cats.contains(&Category::Transaction)); // wal
        assert!(cats.contains(&Category::Consensus)); // quorum
        assert!(cats.contains(&Category::Failure)); // timeout
    }

    #[test]
    fn config_filters_categories() {
        let source = r#"
fn mixed(store: &Store) {
    let guard = store.mutex.lock().unwrap();
    let ballot = prepare_ballot(42);
    store.wal.append(ballot);
}
"#;
        let config = ScanConfig {
            categories: vec![Category::Consensus],
        };
        let result = scan("test.rs", source, &config);
        assert_eq!(result.regions.len(), 1);
        // Should only report consensus, not synchronization or transaction
        assert!(result.regions[0].categories.contains(&Category::Consensus));
        assert!(!result.regions[0]
            .categories
            .contains(&Category::Synchronization));
    }
}
