//! `forge` — Token-saving CLI for Claude Code skill workflows.
//!
//! Single binary with subcommands for summarizing test output, distilling logs,
//! filtering diffs, deduplicating lint output, monitoring logs, validating
//! coverage gates, detecting code smells, checking doc coverage, detecting
//! project stacks, summarizing code structure, running commands with automatic
//! filtering, tracking token savings, and setting up Claude Code hooks.

mod aliases;
mod fmem_skill_ingest;
mod glob;

use fmem_skill_ingest::run_fmem_skill_ingest;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Canonical hook command string. The hook delegates to `frg hook`
/// which auto-detects the right filter at runtime.
const CANONICAL_HOOK_COMMAND: &str = "frg hook 2>/dev/null || true";

/// Hook schema version — bump when the hook format changes.
const HOOK_SCHEMA_VERSION: &str = "1";

#[derive(Parser)]
#[command(name = "frg", version, about = "Token-saving tools for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pretty: bool,

    /// Run as MCP server over stdio (JSON-RPC 2.0)
    #[arg(long)]
    mcp: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Summarize test runner output (cargo test, pytest, jest, go test)
    TestSummary,

    /// Distill build logs into actionable errors and warnings
    LogDistill {
        /// Number of context lines around errors/warnings
        #[arg(short, long, default_value_t = 2)]
        context: usize,
    },

    /// Filter git diff output, skipping noise and collapsing large hunks
    DiffFilter {
        /// Only include files matching these patterns (comma-separated)
        #[arg(long)]
        include: Option<String>,
        /// Max hunk lines before collapsing
        #[arg(long, default_value_t = 80)]
        max_hunk_lines: usize,
        /// Show stats only, no diff content
        #[arg(long)]
        stats_only: bool,
    },

    /// Deduplicate and group lint output by rule
    LintDedup,

    /// Monitor logs for stalls, errors, resource issues, and completion
    LogMonitor {
        /// Consecutive identical lines to flag as stall
        #[arg(long, default_value_t = 5)]
        stall_threshold: usize,
        /// Minimum repeat count to report a line
        #[arg(long, default_value_t = 3)]
        repeat_threshold: usize,
        /// Maximum events to report
        #[arg(long, default_value_t = 50)]
        max_events: usize,
    },

    /// Validate coverage + cyclomatic complexity coupling
    CoverageGate {
        /// Path to lcov coverage file
        #[arg(long)]
        coverage: PathBuf,
        /// Source directory root
        #[arg(long)]
        source: PathBuf,
        /// Baseline coverage percentage
        #[arg(long, default_value_t = 80.0)]
        baseline: f64,
        /// CC threshold for elevated coverage requirement
        #[arg(long, default_value_t = 15)]
        high_cc_threshold: usize,
        /// Coverage required for high-CC functions
        #[arg(long, default_value_t = 90.0)]
        high_cc_coverage: f64,
        /// CC threshold requiring refactor plan
        #[arg(long, default_value_t = 25)]
        critical_cc_threshold: usize,
    },

    /// Detect code smells: long functions, high CC, deep nesting, many params
    SmellDetect {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Max function length before flagging
        #[arg(long, default_value_t = 60)]
        max_lines: usize,
        /// Max cyclomatic complexity before flagging
        #[arg(long, default_value_t = 15)]
        max_cc: usize,
        /// Max nesting depth before flagging
        #[arg(long, default_value_t = 4)]
        max_nesting: usize,
        /// Max parameter count before flagging
        #[arg(long, default_value_t = 5)]
        max_params: usize,
    },

    /// Check documentation coverage for public APIs
    DocCoverage {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },

    /// Detect project type, languages, frameworks, and suggest skills
    ProjectDetect {
        /// Project directory to scan (defaults to current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Include file counts, LOC, module names, and dependencies
        #[arg(long)]
        summary: bool,
    },

    /// Summarize code structure: signatures, types, imports (no bodies)
    Digest {
        /// Source files or directories to summarize
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Output format: json or outline
        #[arg(long, default_value = "outline")]
        format: String,
        /// Token budget: progressively drop detail to fit within N tokens
        #[arg(long)]
        budget: Option<usize>,
        /// Only include files changed since this git ref (e.g. HEAD~3, main)
        #[arg(long)]
        since: Option<String>,
        /// Filter elements to those matching this regex pattern
        #[arg(long)]
        grep: Option<String>,
    },

    /// Run a command and automatically filter its output (proxy mode)
    Run {
        /// Save raw output on failure for debugging
        #[arg(long)]
        tee: bool,
        /// List active filter rules and exit
        #[arg(long)]
        list_filters: bool,
        /// The command and arguments to run
        #[arg(required = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Show token savings analytics
    Gain {
        /// Show output as JSON instead of human-readable
        #[arg(long)]
        json: bool,
    },

    /// Show detailed filter analytics for post-analysis
    Analytics {
        /// Show output as JSON instead of human-readable
        #[arg(long)]
        json: bool,
    },

    /// Clear all analytics data (filter_log and command_log)
    ClearAnalytics,

    /// Install/uninstall Claude Code hooks for automatic command rewriting
    Init {
        /// Install hooks globally (~/.claude/settings.json)
        #[arg(short, long)]
        global: bool,
        /// Remove hooks instead of installing
        #[arg(long)]
        uninstall: bool,
        /// Show current hook status without modifying
        #[arg(long)]
        show: bool,
    },

    /// Scan project and suggest optimization opportunities with estimated savings
    Discover {
        /// Project directory to scan (defaults to current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Extract a single symbol (function/struct/enum) body from a file
    Excerpt {
        /// File:symbol to extract (e.g. src/main.rs:process_data)
        #[arg(required = true)]
        target: String,
        /// Lines of context before the symbol (for doc comments)
        #[arg(short, long, default_value_t = 2)]
        context: usize,
    },

    /// Look up a symbol across source files (LSP hybrid bridge)
    Lookup {
        /// Symbol name to search for
        #[arg(required = true)]
        symbol: String,
        /// Directory to search (defaults to current dir)
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Show or clear session context tracking (files/symbols seen by LLM)
    Context {
        /// Show context for a specific session (default: auto-detect from env)
        #[arg(long)]
        session: Option<String>,
        /// Clear context for this session
        #[arg(long)]
        clear: bool,
    },

    /// Analyze codebase architecture using Design Structure Matrix methodology
    Dsm {
        #[command(subcommand)]
        action: DsmAction,
    },

    /// Scan for concurrency and distributed systems patterns in source code
    ConcurrencyScan {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Only scan specific categories (comma-separated: synchronization,consensus,replication,transaction,failure)
        #[arg(long)]
        categories: Option<String>,
    },

    /// Extract structured outline from a source file (functions, types, imports)
    Outline {
        /// Source file to outline
        #[arg(required = true)]
        file: PathBuf,
    },

    /// Find likely unbounded materialization in disk/storage/query I/O paths
    MaterializationScan {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Include tests and fixtures (default skips them)
        #[arg(long)]
        include_tests: bool,
        /// Maximum findings to return
        #[arg(long, default_value_t = 500)]
        max_findings: usize,
    },

    /// Build per-module dependency tree from project source
    DepTree {
        /// Project directory to analyze (defaults to current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Auto-detect project language and run code formatter
    FormatFix {
        /// Project directory (defaults to current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,

        /// Dry-run: report what would change without modifying files
        #[arg(long)]
        check: bool,
    },

    /// Analyze merge-ability of two git branches (no side effects)
    MergeCheck {
        /// Branch to merge (the feature branch)
        source_branch: String,

        /// Branch to merge into (defaults to current HEAD)
        target_branch: Option<String>,

        /// Test a specific merge strategy: ours, theirs
        #[arg(long)]
        strategy: Option<String>,
    },

    /// Process tool output as a Claude Code hook (thin delegator)
    Hook,

    /// Check ferrosa-memory context for a project (entity counts by type)
    ContextCheck {
        /// Project directory (defaults to current directory)
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Path to the ferrosa-memory MCP binary. If omitted, forge tries the
        /// `FERROSA_MEMORY_MCP_BIN` env var, then `which ferrosa-memory`. If
        /// none resolve, ContextCheck exits silently (best-effort status).
        #[arg(long)]
        mcp_bin: Option<PathBuf>,
    },

    /// Ingest codebase structure into a knowledge graph
    Ingest {
        /// Path to the codebase root
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Path to the ferrosa-memory MCP binary. When omitted, forge uses
        /// the HTTP endpoint from `~/.config/ferrosa-memory.toml`
        /// (`[server] transport = "http"`). The command errors out if
        /// neither is available — it never silently returns extraction
        /// counts as if they were load counts.
        #[arg(long)]
        mcp_bin: Option<PathBuf>,
        /// Session UUID (reads from config if omitted)
        #[arg(long)]
        session: Option<String>,
        /// Tenant UUID (reads from config if omitted)
        #[arg(long)]
        tenant: Option<String>,
        /// Print the extracted IngestReport without writing to ferrosa-memory.
        #[arg(long)]
        dry_run: bool,
    },

    /// Extract one-line descriptions for public entities (LLM-backed)
    #[command(name = "ingest-descriptions")]
    IngestDescriptions {
        /// Path to the codebase root
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Provider: local, openai, anthropic, skip
        #[arg(long, default_value = "local")]
        desc_provider: String,
        /// Local model name (when provider=local)
        #[arg(long, default_value = "qwen2.5-coder:7b")]
        desc_model: String,
        /// Local endpoint (when provider=local). Must be loopback.
        #[arg(long, default_value = "http://localhost:11434")]
        desc_endpoint: String,
        /// Per-call timeout in milliseconds
        #[arg(long, default_value_t = 5000)]
        desc_timeout_ms: u64,
        /// Include private/non-public entities
        #[arg(long)]
        desc_include_private: bool,
        /// Drop descriptions with confidence below this threshold
        #[arg(long, default_value_t = 0.7)]
        desc_min_confidence: f32,
        /// Max concurrent in-flight LLM calls
        #[arg(long, default_value_t = 4)]
        desc_concurrency: usize,
        /// Cap on total LLM calls per run (cost guard)
        #[arg(long, default_value_t = 5000)]
        desc_max_calls: u64,
        /// Fail instead of prompting when the provider probe fails
        #[arg(long)]
        non_interactive: bool,
    },

    /// Ingest a web page into a knowledge graph
    IngestUrl {
        /// URL to fetch and ingest
        url: String,
        /// Crawl depth: 0=single page (default), 1=follow same-domain links, 2=two levels
        #[arg(long, default_value = "0")]
        depth: u32,
        /// Path to the ferrosa-memory MCP binary (see `frg ingest --help`).
        #[arg(long)]
        mcp_bin: Option<PathBuf>,
        /// Session UUID
        #[arg(long)]
        session: Option<String>,
        /// Tenant UUID
        #[arg(long)]
        tenant: Option<String>,
        /// Print report without loading to ferrosa-memory
        #[arg(long)]
        dry_run: bool,
    },

    /// Ingest an academic paper into a knowledge graph
    IngestPaper {
        /// URL, DOI (doi:10.xxx), arxiv ID, or local PDF path
        input: String,
        /// Path to the ferrosa-memory MCP binary (see `frg ingest --help`).
        #[arg(long)]
        mcp_bin: Option<PathBuf>,
        /// Session UUID
        #[arg(long)]
        session: Option<String>,
        /// Tenant UUID
        #[arg(long)]
        tenant: Option<String>,
        /// Print report without loading to ferrosa-memory
        #[arg(long)]
        dry_run: bool,
    },

    /// Ingest corpus markdown distillation files into a knowledge graph
    #[command(name = "ingest-corpus")]
    IngestCorpus {
        /// Path to a corpus markdown file or directory of files
        path: PathBuf,
        /// Path to the ferrosa-memory MCP binary (see `frg ingest --help`).
        #[arg(long)]
        mcp_bin: Option<PathBuf>,
        /// Session UUID
        #[arg(long)]
        session: Option<String>,
        /// Tenant UUID
        #[arg(long)]
        tenant: Option<String>,
        /// Print report without loading to ferrosa-memory
        #[arg(long)]
        dry_run: bool,
    },

    /// Task tracking with CQL-backed kanban board
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },

    /// Ingest the SKILL.md catalog into ferrosa-memory via the `ingest_skill` MCP tool.
    ///
    /// Four-phase pipeline: (A) taxonomy seed from tag-hierarchy.yaml,
    /// (B) per-skill ingest, (C) re-pass for skipped REQUIRES edges,
    /// (D) verification gate. Exits non-zero on any verification failure.
    FmemSkillIngest {
        /// Root directory containing skill categories
        #[arg(long, default_value = "../research/skills")]
        root: PathBuf,
        /// Only ingest skills whose name matches the glob (e.g. `tdd`, `try-*`)
        #[arg(long)]
        filter: Option<String>,
        /// Parse and validate; do not call fmem
        #[arg(long)]
        dry_run: bool,
        /// Session UUID override (defaults to fmem-configured default)
        #[arg(long)]
        session: Option<String>,
        /// Re-ingest even when content_hash matches
        #[arg(long)]
        force: bool,
        /// Space-separated command that launches the fmem MCP server.
        /// Defaults to `fmem --mcp`. Example: `--server 'fmem --mcp --cluster test'`.
        #[arg(long)]
        server: Option<String>,
        /// Log per-skill action with a diff on updates
        #[arg(long)]
        verbose: bool,
    },

    /// Validate Mermaid diagram syntax (reads diagram from stdin)
    MermaidValidate,

    /// Persistent workflow checklist state under .forge/checklists/
    Checklist {
        #[command(subcommand)]
        action: ChecklistAction,
    },

    /// Extract TODO/FIXME/HACK/XXX comments with optional git blame
    TodoExtract {
        /// Directory to scan (defaults to current dir)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Skip git blame attribution (faster, no author/commit fields)
        #[arg(long)]
        no_blame: bool,
        /// Comma-separated subset of TODO,FIXME,HACK,XXX,BUG,NOTE,OPTIMIZE,DEPRECATED
        #[arg(long)]
        kinds: Option<String>,
    },

    /// Scan for leaked API keys, credentials, and private keys
    SecretScan {
        /// Directory to scan (defaults to current dir)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Audit dependency lockfiles for known-vulnerable versions
    DepsAudit {
        /// Project directory to audit (defaults to current dir)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Minimum severity to report: low, medium, high, critical
        #[arg(long, default_value = "medium")]
        min_severity: String,
    },

    /// Scan source for STRIDE attack-pattern regexes
    ThreatScan {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Comma-separated: spoofing,tampering,repudiation,info_disclosure,dos,elevation
        #[arg(long)]
        categories: Option<String>,
        /// Minimum confidence: low, medium, high
        #[arg(long, default_value = "medium")]
        min_confidence: String,
    },

    /// AST scan for swallowed errors, fake success, and runtime mock data
    FailLoudScan {
        /// Source files or directories to scan
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Comma-separated: swallowed_error,fake_success,mock_leak,placeholder_impl,optimistic_status
        #[arg(long)]
        categories: Option<String>,
        /// Minimum confidence: medium, high
        #[arg(long, default_value = "high")]
        min_confidence: String,
    },

    /// Diff two SQL/CQL/Cypher schemas for breaking migrations
    SchemaDiff {
        /// Path to the "before" schema file
        before: PathBuf,
        /// Path to the "after" schema file
        after: PathBuf,
        /// Force dialect: sql, cql, cypher (default: auto-detect)
        #[arg(long)]
        dialect: Option<String>,
    },

    /// Diff public API surface between two source files or directories
    ApiDiff {
        /// Path to the "before" source file or directory
        before: PathBuf,
        /// Path to the "after" source file or directory
        after: PathBuf,
        /// Language hint (auto-detected from file extension if omitted)
        #[arg(long)]
        lang: Option<String>,
    },

    /// Return tool alias map for resolving common tool name mismatches
    ToolAliases {
        /// Output format: json (default) or table
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// Find files matching a glob with structured stats (size, age, generated)
    #[command(name = "glob", alias = "glob-stats")]
    GlobStats {
        /// Glob pattern (e.g. 'src/**/*.rs'). Relative to CWD unless --allow-absolute.
        #[arg(required = true)]
        pattern: String,
        /// Skip files with fewer than N lines
        #[arg(long, default_value_t = 0)]
        min_lines: u64,
        /// Skip files with more than N lines
        #[arg(long, default_value_t = u64::MAX)]
        max_lines: u64,
        /// Skip files smaller than N bytes
        #[arg(long, default_value_t = 0)]
        min_bytes: u64,
        /// Skip files larger than N bytes (default 1 GiB)
        #[arg(long, default_value_t = 1024 * 1024 * 1024)]
        max_bytes: u64,
        /// Only files modified in the last duration (e.g. 7d, 24h, 30m)
        #[arg(long)]
        modified_after: Option<String>,
        /// Only files older than duration (e.g. 30d)
        #[arg(long)]
        modified_before: Option<String>,
        /// Exclude patterns (repeatable or comma-separated). Cannot override secret denylist.
        #[arg(long)]
        exclude: Vec<String>,
        /// Output format: brief (default), json, csv, table
        #[arg(long, default_value = "brief")]
        format: String,
        /// Permit absolute patterns like '/etc/**'
        #[arg(long)]
        allow_absolute: bool,
        /// Max results before truncating (default 10000)
        #[arg(long, default_value_t = 10_000)]
        max_results: usize,
        /// Maximum traversal depth (default 20)
        #[arg(long, default_value_t = 20)]
        max_depth: usize,
        /// Follow symlinks (off by default; use with care)
        #[arg(long)]
        follow_links: bool,
        /// Disable .gitignore / .ignore respect (default: respect them)
        #[arg(long)]
        no_gitignore: bool,
    },
}

#[derive(Subcommand)]
enum ChecklistAction {
    /// Create a new checklist with the given items (comma-separated titles)
    Create {
        /// Checklist name (slug)
        name: String,
        /// Comma-separated item titles
        #[arg(long)]
        items: String,
    },
    /// Create a dependency-aware checklist from a JSON file
    CreateDag {
        /// Checklist name (slug)
        name: String,
        /// JSON file containing a full checklist object
        #[arg(long)]
        file: PathBuf,
    },
    /// List all checklists in .forge/checklists/
    List,
    /// Show a checklist
    Show {
        /// Checklist name
        name: String,
    },
    /// Validate dependency references and cycles
    Validate {
        /// Checklist name
        name: String,
    },
    /// Show pending items whose dependencies are completed
    Ready {
        /// Checklist name
        name: String,
        /// Maximum number of items to return
        #[arg(long)]
        limit: Option<usize>,
        /// Include expired in-progress leases as ready to reclaim
        #[arg(long)]
        include_expired_leases: bool,
    },
    /// Claim ready items for an agent with a lease
    Claim {
        /// Checklist name
        name: String,
        /// Agent identifier
        #[arg(long)]
        agent: String,
        /// Maximum number of items to claim
        #[arg(long, default_value_t = 1)]
        limit: usize,
        /// Lease duration in minutes
        #[arg(long, default_value_t = 60)]
        lease_minutes: i64,
        /// Include expired in-progress leases as reclaimable
        #[arg(long)]
        include_expired_leases: bool,
    },
    /// Set an item's status: pending, in_progress, completed, blocked
    Set {
        /// Checklist name
        name: String,
        /// Item id
        item_id: String,
        /// New status
        status: String,
    },
    /// Attach a note to an item
    Note {
        /// Checklist name
        name: String,
        /// Item id
        item_id: String,
        /// Note text
        text: String,
    },
    /// Release a claimed item back to pending
    Release {
        /// Checklist name
        name: String,
        /// Item id
        item_id: String,
        /// Optional agent id; when set it must match the current claimant
        #[arg(long)]
        agent: Option<String>,
    },
    /// Delete a checklist
    Delete {
        /// Checklist name
        name: String,
    },
}

#[derive(Subcommand)]
enum DsmAction {
    /// Extract dependency edges from source code
    Extract {
        /// Project directory to analyze
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Language: java, rust, python, go, typescript, elixir, or auto
        #[arg(long, default_value = "auto")]
        language: String,
        /// Granularity: summary or full
        #[arg(long, default_value = "summary")]
        level: String,
        /// Only include elements matching this prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Detect cross-language FFI/IPC calls
        #[arg(long)]
        cross_language: bool,
    },
    /// Build DSM matrix from edges (stdin: JSON edges or DOT)
    Build {
        /// Granularity: summary or full
        #[arg(long, default_value = "summary")]
        level: String,
        /// Use numerical (weighted) matrix
        #[arg(long)]
        numerical: bool,
    },
    /// Full analysis: extract + partition + cluster + metrics + suggestions
    Analyze {
        /// Project directory to analyze
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Granularity: summary or full
        #[arg(long, default_value = "summary")]
        level: String,
        /// Only include elements matching this prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Path to modules.toml for user-directed analysis
        #[arg(long)]
        modules: Option<PathBuf>,
        /// Number of clustering runs
        #[arg(long, default_value_t = 5)]
        cluster_runs: usize,
        /// Max clustering iterations per run
        #[arg(long, default_value_t = 10000)]
        cluster_iterations: usize,
        /// Random seed for deterministic results
        #[arg(long)]
        seed: Option<u64>,
        /// Output format: mermaid, markdown, svg, html, json, csv
        #[arg(long, default_value = "markdown")]
        format: String,
        /// Detect cross-language FFI/IPC calls
        #[arg(long)]
        cross_language: bool,
    },
    /// Generate refactoring suggestions from analysis results (stdin: JSON report)
    Suggest {
        /// Output format: mermaid, markdown, json
        #[arg(long, default_value = "markdown")]
        format: String,
        /// Minimum priority: critical, high, medium, low
        #[arg(long, default_value = "low")]
        min_priority: String,
    },
    /// Find dead code (unreachable declarations)
    DeadCode {
        /// Project directory to analyze
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Granularity: summary or full
        #[arg(long, default_value = "full")]
        level: String,
        /// Only include elements matching this prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Minimum confidence: definite, possible, all
        #[arg(long, default_value = "all")]
        min_confidence: String,
        /// Output format: markdown, json
        #[arg(long, default_value = "markdown")]
        format: String,
        /// Include test code in analysis
        #[arg(long)]
        include_tests: bool,
    },
    /// Generate architecture enforcement tests
    Enforce {
        /// Path to modules.toml
        #[arg(long, required = true)]
        modules: PathBuf,
        /// Test framework: archunit, cargo-test, pytest, generic
        #[arg(long, default_value = "generic")]
        framework: String,
        /// Output directory for generated test files
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
        /// Strict mode: fail on any violation
        #[arg(long)]
        strict: bool,
    },
}

// ---------------------------------------------------------------------------
// Task subcommand actions
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
enum TaskAction {
    /// Create a new task
    Create {
        /// Task title
        #[arg(long, required = true)]
        title: String,
        /// Task body / description
        #[arg(long)]
        body: Option<String>,
        /// Assignee name
        #[arg(long)]
        assignee: Option<String>,
        /// Priority (default 50)
        #[arg(long)]
        priority: Option<i32>,
        /// Workspace kind (e.g. "worktree", "branch")
        #[arg(long)]
        workspace: Option<String>,
        /// Creator name (defaults to "agent")
        #[arg(long)]
        created_by: Option<String>,
        /// Workspace path (e.g. repo root) — the per-repo key for /whats-next and /roadmap
        #[arg(long)]
        workspace_path: Option<String>,
        /// Metadata as a JSON string (e.g. '{"source":"stop-hook"}')
        #[arg(long)]
        metadata: Option<String>,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Update an existing task
    Update {
        /// Task ID (e.g. t_1a2b3c4d)
        task_id: String,
        /// New status: triage, ready, in_progress, blocked, complete, archived
        #[arg(long)]
        status: Option<String>,
        /// New assignee
        #[arg(long)]
        assignee: Option<String>,
        /// New priority
        #[arg(long)]
        priority: Option<i32>,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// New body
        #[arg(long)]
        body: Option<String>,
        /// Block reason (use with --status blocked)
        #[arg(long)]
        block_reason: Option<String>,
        /// Result summary
        #[arg(long)]
        result: Option<String>,
        /// Task summary
        #[arg(long)]
        summary: Option<String>,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Get a task with its links and comments
    Get {
        /// Task ID
        task_id: String,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// List tasks with optional filters
    List {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
        /// Filter by assignee
        #[arg(long)]
        assignee: Option<String>,
        /// Minimum priority
        #[arg(long)]
        priority_gte: Option<i32>,
        /// Maximum priority
        #[arg(long)]
        priority_lte: Option<i32>,
        /// Maximum results to return
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Link two tasks (parent → child)
    Link {
        /// Parent task ID
        parent_id: String,
        /// Child task ID
        child_id: String,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Remove a link between two tasks
    Unlink {
        /// Parent task ID
        parent_id: String,
        /// Child task ID
        child_id: String,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Add a comment to a task
    Comment {
        /// Task ID
        task_id: String,
        /// Comment text
        #[arg(long, required = true)]
        body: String,
        /// Author name (defaults to "agent")
        #[arg(long, default_value = "agent")]
        author: String,
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
    /// Show the kanban board
    Board {
        /// CQL host (overrides FORGE_CQL_HOST / .forge/config.toml; default 127.0.0.1:9042)
        #[arg(long)]
        cql_host: Option<String>,
    },
}

/// Run a command as a subprocess and return (stdout+stderr, exit_code).
fn run_command(args: &[String]) -> anyhow::Result<(String, i32)> {
    use std::process::Command;

    let (program, cmd_args) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("No command provided"))?;

    let output = Command::new(program)
        .args(cmd_args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to execute '{}': {}", program, e))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        combined.push_str(&stderr);
    }

    let exit_code = output.status.code().unwrap_or(1);
    Ok((combined, exit_code))
}

/// Apply a named filter to command output.
fn apply_filter(filter: &str, input: &str, pretty: bool) -> anyhow::Result<String> {
    match filter {
        "test-summary" => {
            let summary = forge_test_summary::parser::parse(input)?;
            forge_shared::emit_json(&summary, pretty)
        }
        "lint-dedup" => {
            let result = forge_lint_dedup::dedup::dedup(input);
            forge_shared::emit_json(&result, pretty)
        }
        "diff-filter" => {
            let config = forge_diff_filter::filter::FilterConfig::default();
            let result = forge_diff_filter::filter::filter_diff(input, &config);
            Ok(result.output)
        }
        "log-distill" => {
            let result = forge_log_distill::distiller::distill(input, 2);
            forge_shared::emit_json(&result, pretty)
        }
        _ => Ok(input.to_string()),
    }
}

/// Result of running a command through the filter pipeline.
struct FilterResult {
    filter_name: String,
    output: String,
    success: bool,
    error: Option<String>,
    duration_ms: u64,
}

/// Detect the appropriate filter and apply it to command output.
/// On parse failure, falls back to the registry's fallback filter.
fn filter_output(
    registry: &forge_shared::filters::FilterRegistry,
    command: &str,
    raw_output: &str,
    pretty: bool,
) -> FilterResult {
    let filter_name = registry.detect(command).to_string();
    let start = std::time::Instant::now();

    match apply_filter(&filter_name, raw_output, pretty) {
        Ok(output) => FilterResult {
            filter_name,
            output,
            success: true,
            error: None,
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(e) => {
            // If a specialized filter failed, try the fallback filter
            if filter_name != registry.fallback {
                if let Ok(fallback_output) = apply_filter(&registry.fallback, raw_output, pretty) {
                    return FilterResult {
                        filter_name,
                        output: fallback_output,
                        success: false,
                        error: Some(e.to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }
            }
            // Last resort: return raw output
            FilterResult {
                filter_name,
                output: raw_output.to_string(),
                success: false,
                error: Some(e.to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
    }
}

/// Generate Claude Code hook configuration JSON using the thin delegator pattern.
fn generate_hook_config() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Read",
                    "hooks": [
                        {
                            "type": "command",
                            "command": CANONICAL_HOOK_COMMAND
                        }
                    ]
                }
            ],
            "PostToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {
                            "type": "command",
                            "command": CANONICAL_HOOK_COMMAND
                        }
                    ]
                },
                {
                    "matcher": "Read",
                    "hooks": [
                        {
                            "type": "command",
                            "command": CANONICAL_HOOK_COMMAND
                        }
                    ]
                }
            ]
        }
    })
}

fn handle_dsm(action: DsmAction, pretty: bool) -> anyhow::Result<()> {
    use forge_dsm_analyze::cluster::{cluster, ClusterConfig};
    use forge_dsm_analyze::cycles::find_cycles;
    use forge_dsm_analyze::extract::multi::MultiExtractor;
    use forge_dsm_analyze::extract::{ExtractConfig, GranularityLevel};
    use forge_dsm_analyze::matrix::DsmMatrix;
    use forge_dsm_analyze::metrics::compute_metrics;
    use forge_dsm_analyze::partition::partition;
    use forge_dsm_analyze::report::{render, DsmReport, OutputFormat};
    use forge_dsm_analyze::suggest::generate_suggestions;

    fn parse_level(s: &str) -> GranularityLevel {
        match s {
            "full" => GranularityLevel::Full,
            _ => GranularityLevel::Summary,
        }
    }

    fn parse_format(s: &str) -> OutputFormat {
        match s {
            "mermaid" => OutputFormat::Mermaid,
            "svg" => OutputFormat::Svg,
            "html" => OutputFormat::Html,
            "json" => OutputFormat::Json,
            "csv" => OutputFormat::Csv,
            _ => OutputFormat::Markdown,
        }
    }

    match action {
        DsmAction::Extract {
            dir,
            language,
            level,
            prefix,
            cross_language,
        } => {
            let config = ExtractConfig {
                level: parse_level(&level),
                prefix_filter: prefix,
                exclude_patterns: vec![],
                detect_cross_language: cross_language,
            };
            let extractor = MultiExtractor::new();
            let edges = if language == "auto" {
                extractor.extract_all(&dir, &config)?
            } else {
                // Use specific extractor
                extractor.extract_all(&dir, &config)?
            };
            println!("{}", forge_shared::emit_json(&edges, pretty)?);
        }

        DsmAction::Build {
            level: _,
            numerical,
        } => {
            let input = forge_shared::read_stdin()?;
            // Try parsing as JSON edges first, fall back to DOT
            let edges: Vec<forge_dsm_analyze::extract::Edge> = match serde_json::from_str(&input) {
                Ok(e) => e,
                Err(_) => forge_dsm_analyze::extract::dot_parser::parse_dot(&input)?,
            };
            let matrix = if numerical {
                DsmMatrix::from_edges_numerical(&edges)
            } else {
                DsmMatrix::from_edges(&edges)
            };
            println!("{}", forge_shared::emit_json(&matrix, pretty)?);
        }

        DsmAction::Analyze {
            dir,
            level,
            prefix,
            modules,
            cluster_runs,
            cluster_iterations,
            seed,
            format,
            cross_language,
        } => {
            let config = ExtractConfig {
                level: parse_level(&level),
                prefix_filter: prefix,
                exclude_patterns: vec![],
                detect_cross_language: cross_language,
            };

            // Extract
            let extractor = MultiExtractor::new();
            let edges = extractor.extract_all(&dir, &config)?;
            if edges.is_empty() {
                anyhow::bail!("No dependencies extracted. Check that the project has source files and any required build tools are installed.");
            }
            let edge_count = edges.len();

            // Build matrix
            let matrix = DsmMatrix::from_edges(&edges);

            // Analyze
            let cycles = find_cycles(&matrix);
            let cluster_config = ClusterConfig {
                max_iterations: cluster_iterations,
                num_runs: cluster_runs,
                seed,
                pow_cc: 1.0,
            };
            let clusters = cluster(&matrix, &cluster_config);
            let metrics = compute_metrics(&matrix, &cycles, &clusters);
            let part = partition(&matrix);

            // Directed analysis if modules.toml provided
            let directed = if let Some(modules_path) = modules {
                let toml_content = std::fs::read_to_string(&modules_path)?;
                let dir_config = forge_dsm_analyze::directed::parse_modules_toml(&toml_content)?;
                Some(forge_dsm_analyze::directed::directed_analysis(
                    &matrix,
                    &dir_config,
                    &cycles,
                ))
            } else {
                None
            };

            // Suggestions
            let suggestions = generate_suggestions(
                &matrix,
                &cycles,
                &clusters,
                &metrics,
                &part,
                directed.as_ref(),
            );

            // Build report
            let project_name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string());

            let report = DsmReport {
                project_name,
                element_count: matrix.size(),
                edge_count,
                metrics,
                cycles,
                clusters,
                partition: part,
                suggestions,
                directed,
            };

            let output_format = parse_format(&format);
            println!("{}", render(&report, &output_format));
        }

        DsmAction::Suggest {
            format,
            min_priority,
        } => {
            let input = forge_shared::read_stdin()?;
            let mut report: DsmReport = serde_json::from_str(&input)?;
            let min_ord = match min_priority.to_lowercase().as_str() {
                "critical" => 0,
                "high" => 1,
                "medium" => 2,
                _ => 3,
            };
            report.suggestions.retain(|s| {
                let ord = match s.priority {
                    forge_dsm_analyze::suggest::Priority::Critical => 0,
                    forge_dsm_analyze::suggest::Priority::High => 1,
                    forge_dsm_analyze::suggest::Priority::Medium => 2,
                    forge_dsm_analyze::suggest::Priority::Low => 3,
                };
                ord <= min_ord
            });
            let output_format = parse_format(&format);
            println!("{}", render(&report, &output_format));
        }

        DsmAction::DeadCode {
            dir,
            level,
            prefix,
            min_confidence,
            format,
            include_tests,
        } => {
            use forge_dsm_analyze::dead_code::{find_dead_code, Confidence};
            let config = ExtractConfig {
                level: parse_level(&level),
                prefix_filter: prefix,
                exclude_patterns: vec![],
                detect_cross_language: false,
            };

            let multi = forge_dsm_analyze::extract::multi::MultiExtractor::new();
            let (declarations, references) = multi.extract_declarations(&dir, &config)?;

            let mut report = find_dead_code(&declarations, &references, include_tests);

            // Filter by confidence
            match min_confidence.as_str() {
                "definite" => {
                    report
                        .findings
                        .retain(|f| f.confidence == Confidence::Definite);
                }
                "possible" => {
                    // keep both definite and possible
                }
                _ => {
                    // "all" - keep everything
                }
            }

            match format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                _ => {
                    // Markdown output
                    println!("# Dead Code Analysis\n");
                    println!("| Metric | Value |");
                    println!("|--------|-------|");
                    println!("| Total declarations | {} |", report.total_declarations);
                    println!("| Entry points | {} |", report.total_entry_points);
                    println!("| Reachable symbols | {} |", report.total_reachable);
                    println!("| Definitely dead | {} |", report.dead_definite);
                    println!("| Possibly dead | {} |", report.dead_possible);
                    println!();

                    if !report.findings.is_empty() {
                        println!("## Findings\n");
                        println!("| Confidence | Kind | Name | File | Line | Reason |");
                        println!("|------------|------|------|------|------|--------|");
                        for f in &report.findings {
                            println!(
                                "| {:?} | {:?} | `{}` | {} | {} | {} |",
                                f.confidence,
                                f.declaration.kind,
                                f.declaration.name,
                                f.declaration.file,
                                f.declaration.line,
                                f.reason,
                            );
                        }
                    } else {
                        println!("No dead code found!");
                    }
                }
            }
        }

        DsmAction::Enforce {
            modules,
            framework,
            output_dir,
            strict,
        } => {
            let input = forge_shared::read_stdin()?;
            let report: DsmReport = serde_json::from_str(&input)?;
            let toml_content = std::fs::read_to_string(&modules)?;
            let dir_config = forge_dsm_analyze::directed::parse_modules_toml(&toml_content)?;

            let fw = match framework.as_str() {
                "archunit" => forge_dsm_analyze::enforce::TestFramework::ArchUnit,
                "cargo-test" => forge_dsm_analyze::enforce::TestFramework::CargoTest,
                "pytest" => forge_dsm_analyze::enforce::TestFramework::Pytest,
                _ => forge_dsm_analyze::enforce::TestFramework::Generic,
            };

            let enforce_config = forge_dsm_analyze::enforce::EnforcementConfig {
                framework: fw,
                modules: dir_config,
                strict,
            };

            let directed = report
                .directed
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Report must include directed analysis results"))?;

            let files = forge_dsm_analyze::enforce::generate_enforcement_tests(
                &enforce_config,
                directed,
                &report.partition,
            );

            for file in &files {
                let path = output_dir.join(&file.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, &file.content)?;
                eprintln!("Generated: {}", path.display());
            }
        }
    }

    Ok(())
}

// ── MCP registration macros ──────────────────────────────────────────────────
//
// Three macros that cut per-tool registration from ~20 lines to ~5 lines.
//
// * `register_tool!`       — fully custom schema + handler
// * `register_path_tool!`  — tools that take an optional `path` param
// * `register_stdin_tool!` — tools that accept text `input` and return JSON

/// Fully custom tool registration (tier 1 — always visible).
macro_rules! register_tool {
    ($server:expr, $name:expr, $desc:expr, $schema:expr, $handler:expr) => {
        $server.register_tool(
            forge_mcp_server::ToolDef {
                name: $name.to_string(),
                description: $desc.to_string(),
                input_schema: $schema,
                tier: 1,
                ..Default::default()
            },
            std::sync::Arc::new($handler),
        );
    };
}

/// Tool that takes an optional `path` parameter (defaults to cwd).
/// The handler receives the full `serde_json::Value` args.
macro_rules! register_path_tool {
    ($server:expr, $name:expr, $desc:expr, $handler:expr) => {
        register_tool!(
            $server,
            $name,
            $desc,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory or file path to operate on (defaults to current working directory)"}
                }
            }),
            $handler
        );
    };
}

/// Tool that accepts text via an `input` parameter (replacing stdin).
/// For tools with no extra properties beyond `input`.
macro_rules! register_stdin_tool {
    ($server:expr, $name:expr, $desc:expr, $handler:expr) => {
        register_tool!(
            $server,
            $name,
            $desc,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string", "description": "Raw output text to parse"}
                },
                "required": ["input"]
            }),
            $handler
        );
    };
}

/// Build a JSON schema with `input` (required) plus additional properties.
/// Returns a `serde_json::Value`.
fn stdin_schema_with(extra: serde_json::Value) -> serde_json::Value {
    let mut props = serde_json::json!({
        "input": {"type": "string", "description": "Raw output text to parse"}
    });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            props[k] = v.clone();
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": ["input"]
    })
}

/// Record analytics for an MCP tool invocation that wraps a shell command.
/// `raw_input_bytes` is the size of the raw command output before structuring.
/// `output_json` is the structured JSON response sent to the client.
fn record_mcp_analytics(tool_name: &str, raw_input_bytes: usize, output_json: &str, dir: &str) {
    if let Ok(db_path) = forge_shared::tracking::default_db_path() {
        if let Ok(conn) = forge_shared::tracking::open_db(&db_path) {
            let _ = forge_shared::tracking::record_filter(
                &conn,
                &forge_shared::tracking::FilterRecord {
                    command: &format!("mcp:{tool_name}"),
                    filter_name: tool_name,
                    mode: forge_shared::tracking::InvocationMode::Mcp,
                    project_dir: Some(dir),
                    input_bytes: raw_input_bytes,
                    output_bytes: output_json.len(),
                    duration_ms: 0,
                    filter_success: true,
                    error_message: None,
                    exit_code: 0,
                    tool_version: env!("CARGO_PKG_VERSION"),
                },
            );
        }
    }
}

/// Run forge as an MCP server over stdio, exposing all commands as tools.
fn run_mcp_server() -> anyhow::Result<()> {
    use forge_mcp_server::{McpServer, ToolDef};

    // MCP calls should stay responsive and avoid long-lived helper subprocesses
    // unless explicitly requested by the operator.
    if std::env::var_os("FORGE_DISABLE_LSP").is_none() {
        std::env::set_var("FORGE_DISABLE_LSP", "1");
    }

    let version = env!("CARGO_PKG_VERSION");
    let mut server = McpServer::new("forge", version);

    // Auto-detect project stack for tier-2 tool filtering.
    // Runs project_detect on cwd to determine which language-specific tools to expose.
    let detected_stacks: Vec<String> = {
        let cwd = std::path::PathBuf::from(".");
        let result = forge_project_detect::detector::detect(&cwd);
        result
            .languages
            .iter()
            .map(|l| l.name.to_lowercase())
            .collect()
    };
    server.set_detected_stacks(detected_stacks.clone());

    // project_detect
    register_path_tool!(
        server,
        "project_detect",
        "Detect project type, languages, frameworks, and matching skills for a directory. Use this when starting work on an unfamiliar project, onboarding to a codebase, or when you need to know what languages and build systems are present before running build/test/lint commands. Returns a structured JSON object with detected languages, frameworks, build files, and recommended forge commands. Does not modify any files.",
        |args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = std::path::PathBuf::from(dir);
            let result = forge_project_detect::detector::detect(&path);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // project_summary (project-detect --summary)
    register_path_tool!(
        server,
        "project_summary",
        "Get a comprehensive project structure overview including file counts by language, lines of code, module organization, and dependency information. Use this when you need a high-level understanding of a codebase's size, shape, and technology mix — for example, before planning a refactor, writing an architecture document, or answering questions about project scope. Returns both detection results (languages, frameworks) and a structural summary (directory tree stats, LOC breakdown). More detailed than project_detect; use project_detect when you only need language/framework identification.",
        |args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = std::path::PathBuf::from(dir);
            let detect_result = forge_project_detect::detector::detect(&path);
            let summary = forge_project_detect::summary::summarize(&path);
            let combined = serde_json::json!({
                "detection": detect_result,
                "summary": summary,
            });
            serde_json::to_string_pretty(&combined).map_err(|e| e.to_string())
        }
    );

    // find_definition (lookup)
    register_tool!(
        server,
        "find_definition",
        "Find the file and line where a function, module, class, struct, or type is defined across all source files in a project. Use this when you need to locate a symbol definition without knowing which file it lives in — for example, before reading or editing a function, or when navigating an unfamiliar codebase. Returns the file path, line number, and symbol kind for each match. Searches all recognized source files (Rust, Python, Elixir, Go, TypeScript, Java, C/C++, etc.) respecting .gitignore. Does not search inside function bodies or comments — only declarations.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Symbol name to search for, e.g. 'process_data' or 'MyClass'"},
                "path": {"type": "string", "description": "Root directory to search recursively (defaults to current working directory)"}
            },
            "required": ["name"]
        }),
        |args| {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: name")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = std::path::PathBuf::from(dir);
            let result =
                forge_digest::lookup::lookup_symbol(name, &path).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // module_outline (outline)
    register_tool!(
        server,
        "module_outline",
        "Extract all function signatures, struct/class definitions, type aliases, and module structure from a single source file — without function bodies. Use this to understand a file's public API and organization before reading the full source, or when you need to see what a module exports without consuming tokens on implementation details. Returns a structured list of declarations with names, types, visibility, and line numbers. Supports Rust, Python, Elixir, Go, TypeScript, Java, C/C++, and more.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {"type": "string", "description": "Absolute or relative path to a source file, e.g. 'src/main.rs' or 'lib/app/router.ex'"}
            },
            "required": ["file"]
        }),
        |args| {
            let file = args
                .get("file")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: file")?;
            let source = std::fs::read_to_string(file)
                .map_err(|e| format!("Cannot read file {}: {}", file, e))?;
            let result = forge_outline::outline(file, &source);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // dependency_tree (dep-tree)
    register_path_tool!(
        server,
        "dependency_tree",
        "Build a per-module dependency map showing imports, uses, and aliases between source files in a project. Use this when analyzing coupling between modules, planning a refactor that may affect dependents, or understanding how components connect before making architectural changes. Returns a structured JSON with each module's incoming and outgoing dependencies. Respects .gitignore and supports multi-language projects.",
        |args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = std::path::PathBuf::from(dir);
            let result = forge_dep_tree::build_dep_tree(&path).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // digest
    register_tool!(
        server,
        "digest",
        "Produce a token-efficient structural summary of source code — function signatures, type definitions, imports, and module layout — without including function bodies. Use this when you need to understand a file or directory's code structure while minimizing context window usage. Ideal for large codebases where reading full files would be wasteful. For a single file, returns its structural outline; for a directory, returns outlines for all source files. The optional budget parameter enables progressive detail dropping to fit a token budget. More comprehensive than module_outline (which handles one file); use digest for multi-file summaries.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path or directory to summarize, e.g. 'src/' or 'lib/app.ex'"},
                "budget": {"type": "integer", "description": "Maximum token budget — when set, progressively drops detail (private items, then parameters, then types) to fit within this limit. Omit for full detail."}
            },
            "required": ["path"]
        }),
        |args| {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: path")?;
            let path = std::path::PathBuf::from(path_str);
            let budget = args
                .get("budget")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);

            let mut digests = Vec::new();
            if path.is_file() {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Cannot read {}: {}", path_str, e))?;
                digests.push(forge_digest::summarizer::summarize(path_str, &source));
            } else if path.is_dir() {
                for entry in ignore::WalkBuilder::new(&path).build().flatten() {
                    if entry.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Ok(source) = std::fs::read_to_string(entry.path()) {
                            let digest = forge_digest::summarizer::summarize(
                                &entry.path().display().to_string(),
                                &source,
                            );
                            if !digest.elements.is_empty() {
                                digests.push(digest);
                            }
                        }
                    }
                }
            }

            let output = if let Some(b) = budget {
                forge_digest::summarizer::format_multi_outline_budgeted(&digests, b)
            } else {
                forge_digest::summarizer::format_multi_outline(&digests)
            };
            Ok(output)
        }
    );

    // excerpt
    register_tool!(
        server,
        "excerpt",
        "Extract the complete source code of a single symbol (function, struct, enum, class, or method) from a file by name. Use this when you need to read one specific function or type definition without loading the entire file — significantly more token-efficient than reading a large file when you only need one symbol. Returns the full body including documentation comments. Use find_definition first if you don't know which file contains the symbol.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "description": "Colon-separated file path and symbol name to extract. Format: 'filepath:symbol_name', e.g. 'src/main.rs:process_data' or 'lib/app/router.ex:call'"}
            },
            "required": ["target"]
        }),
        |args| {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: target")?;
            let parts: Vec<&str> = target.rsplitn(2, ':').collect();
            if parts.len() != 2 {
                return Err("Target must be in format file:symbol".to_string());
            }
            let (symbol, file) = (parts[0], parts[1]);
            let source = std::fs::read_to_string(file)
                .map_err(|e| format!("Cannot read {}: {}", file, e))?;
            match forge_digest::lookup::extract_and_format(file, &source, symbol) {
                Some(text) => Ok(text),
                None => Err(format!("Symbol '{}' not found in {}", symbol, file)),
            }
        }
    );

    // smell_detect
    register_path_tool!(
        server,
        "smell_detect",
        "Scan source files for code smells: functions exceeding 60 lines, high cyclomatic complexity (CC >= 15), deep nesting (> 4 levels), and excessive parameters (> 5). Use this before refactoring to identify problem areas, during code review to flag quality issues, or as part of a CI quality gate. Returns a structured JSON array of findings with file path, line number, function name, smell type, and severity. Accepts a single file or an entire directory (scans recursively, respects .gitignore). Returns an empty array when no smells are found.",
        |args| {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: path")?;
            let path = std::path::PathBuf::from(path_str);
            let config = forge_smell_detect::detector::DetectConfig::default();
            let mut reports = Vec::new();

            if path.is_file() {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Cannot read {}: {}", path_str, e))?;
                let report = forge_smell_detect::detector::detect(path_str, &source, &config);
                if !report.smells.is_empty() {
                    reports.push(report);
                }
            } else if path.is_dir() {
                for entry in ignore::WalkBuilder::new(&path).build().flatten() {
                    if entry.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Ok(source) = std::fs::read_to_string(entry.path()) {
                            let report = forge_smell_detect::detector::detect(
                                &entry.path().display().to_string(),
                                &source,
                                &config,
                            );
                            if !report.smells.is_empty() {
                                reports.push(report);
                            }
                        }
                    }
                }
            }

            serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())
        }
    );

    // format_fix
    register_tool!(
        server,
        "format_fix",
        "Auto-detect the project language and run the appropriate code formatter (rustfmt, black/ruff, mix format, gofmt, prettier, clang-format). Use this after writing or editing code to ensure consistent formatting, or before committing to fix style violations. Returns a structured JSON list of files that were modified. Set check=true for a dry-run that reports unformatted files without modifying them. Does not require you to know which formatter to use — language detection is automatic.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "dir": {"type": "string", "description": "Project root directory to format (defaults to current working directory)"},
                "check": {"type": "boolean", "description": "When true, only report unformatted files without modifying them (dry-run mode). Defaults to false."}
            }
        }),
        |args| {
            let dir = args.get("dir").and_then(|v| v.as_str()).unwrap_or(".");
            let check = args.get("check").and_then(|v| v.as_bool()).unwrap_or(false);
            let path = std::path::PathBuf::from(dir);
            let result =
                forge_format_fix::format_fix(&path, check).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // merge_check
    register_tool!(server, "merge_check",
        "Analyze whether two git branches can merge cleanly without actually performing the merge. Use this before merging or creating a pull request to identify conflicts early, or when deciding between merge strategies. Reports each conflicting file, the conflict type, and suggested auto-resolution strategies (ours, theirs). Returns a structured JSON with merge_clean (boolean), conflict count, and per-file conflict details. Does not modify the working tree or any branches.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "source_branch": {"type": "string", "description": "The feature branch to merge from, e.g. 'feature/auth-refactor'"},
                "target_branch": {"type": "string", "description": "The branch to merge into (defaults to current HEAD if omitted), e.g. 'main'"},
                "strategy": {"type": "string", "description": "Test a specific merge strategy: 'ours' (keep target) or 'theirs' (keep source)"}
            },
            "required": ["source_branch"]
        }),
        |args| {
            let source = args.get("source_branch").and_then(|v| v.as_str())
                .ok_or("Missing required parameter: source_branch")?;
            let target = args.get("target_branch").and_then(|v| v.as_str());
            let dir = std::env::current_dir().map_err(|e| e.to_string())?;
            let result = forge_merge_check::merge_check(&dir, source, target)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // ── devtools: language build/test/lint wrappers ────────────────────────

    // cargo (Rust)
    server.register_tool(
        ToolDef {
            name: "cargo".to_string(),
            description: "Run Rust/Cargo commands with structured JSON output. Use this instead of running `cargo` via Bash — it parses compiler output into structured errors/warnings with file paths, line numbers, and error codes, saving 80-90% of tokens compared to raw cargo output. Supports build, check, test, clippy, and fmt_check subcommands. On failure, the response includes a `hint` field with actionable recovery steps (e.g., 'run cargo fmt' for format violations). Returns JSON with `success` (boolean), `errors` (array of structured diagnostics), `warnings` (array), and `summary` fields.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["build", "check", "test", "clippy", "fmt_check"], "description": "Cargo subcommand: 'build' compiles the project, 'check' type-checks without codegen, 'test' runs the test suite, 'clippy' runs the Rust linter, 'fmt_check' checks formatting without modifying files"},
                    "path": {"type": "string", "description": "Rust project directory containing Cargo.toml (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments passed to cargo, e.g. '--release' or '--lib' or '-- --test-threads=1'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                },
                "required": ["command"]
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::cargo::run(cmd, dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("cargo", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // clippy — standalone convenience wrapper for cargo clippy
    server.register_tool(
        ToolDef {
            name: "clippy".to_string(),
            description: "Run Rust Clippy linter with structured JSON output. Use this instead of running `cargo clippy` via Bash — it parses clippy output into structured errors/warnings with file paths, line numbers, and lint codes, saving 80-90% of tokens compared to raw output. On failure, the response includes a `hint` field with actionable recovery steps. Returns JSON with `success` (boolean), `errors` (array of structured diagnostics), `warnings` (array), and `summary` fields. This is a convenience shortcut for the `cargo` tool with `command: \"clippy\"`.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Rust project directory containing Cargo.toml (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments passed to cargo clippy, e.g. '-- -W clippy::pedantic' or '--all-targets'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                }
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::cargo::run("clippy", dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("clippy", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // go_tools
    server.register_tool(
        ToolDef {
            name: "go_tools".to_string(),
            description: "Run Go toolchain commands with structured JSON output. Use this instead of running `go` via Bash — it parses compiler and test output into structured errors with file paths and line numbers, saving 80-90% of tokens compared to raw output. Supports build, test, vet, fmt_check, and mod_tidy subcommands. On failure, the response includes a `hint` field with actionable recovery steps. Returns JSON with `success` (boolean), `errors` (array of structured diagnostics), and `summary` fields.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["build", "test", "vet", "fmt_check", "mod_tidy"], "description": "Go subcommand: 'build' compiles packages, 'test' runs tests, 'vet' reports suspicious constructs, 'fmt_check' checks gofmt compliance, 'mod_tidy' cleans up go.mod/go.sum"},
                    "path": {"type": "string", "description": "Go project directory containing go.mod (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments passed to go, e.g. '-v' or '-run TestFoo' or '-count=1'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                },
                "required": ["command"]
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::go::run(cmd, dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("go_tools", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // dotnet (C#/.NET)
    server.register_tool(
        ToolDef {
            name: "dotnet".to_string(),
            description: "Run .NET/C# commands with structured JSON output. Use this instead of running `dotnet` via Bash — it parses Roslyn compiler output into structured errors/warnings with file paths, line numbers, and error codes, saving 80-90% of tokens compared to raw dotnet output. Supports build, test, and format_check subcommands. On failure, the response includes a `hint` field with actionable recovery steps. Returns JSON with `success` (boolean), `errors` (array of structured diagnostics), `warnings` (array), and `summary` fields.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["build", "test", "format_check"], "description": "dotnet subcommand: 'build' compiles the project, 'test' runs the test suite, 'format_check' checks formatting without modifying files"},
                    "path": {"type": "string", "description": ".NET project directory containing .csproj or .sln (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments passed to dotnet, e.g. '--configuration Release' or '--filter FullyQualifiedName~MyTest'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                },
                "required": ["command"]
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::dotnet::run(cmd, dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("dotnet", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // mix_compile (Elixir)
    server.register_tool(
        ToolDef {
            name: "mix_compile".to_string(),
            description: "Compile an Elixir project with `mix compile --all-warnings` and return structured JSON output. Use this instead of running `mix compile` via Bash — it parses compiler output into structured errors/warnings with file paths, line numbers, and warning categories. On failure, the response includes a `hint` field with actionable recovery steps. Returns JSON with `success` (boolean), `errors` and `warnings` arrays with file:line locations.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project directory (defaults to cwd)"},
                    "container": {"type": "string", "description": "Docker container to run inside"}
                }
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::elixir::compile(dir, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("mix_compile", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // mix_test (Elixir)
    server.register_tool(
        ToolDef {
            name: "mix_test".to_string(),
            description: "Run Elixir tests with `mix test` and return structured JSON output. Use this instead of running `mix test` via Bash — it parses test output into structured pass/fail/skip counts and per-failure details including test name, file, line number, assertion message, and expected vs. actual values. On failure, the response includes a `hint` field with recovery steps. Supports running a specific test file or the full suite. Returns JSON with `success`, `passed`, `failed`, `skipped` counts, and a `failures` array.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Elixir project directory containing mix.exs (defaults to current working directory)"},
                    "file": {"type": "string", "description": "Specific test file to run, e.g. 'test/my_module_test.exs' or 'test/my_module_test.exs:42' for a specific line"},
                    "args": {"type": "string", "description": "Additional arguments passed to mix test, e.g. '--trace' or '--seed 12345' or '--max-failures 1'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                }
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let file = args.get("file").and_then(|v| v.as_str());
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::elixir::test(dir, file, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("mix_test", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // mix_format_check (Elixir)
    server.register_tool(
        ToolDef {
            name: "mix_format_check".to_string(),
            description: "Check Elixir code formatting with `mix format --check-formatted`. Use this to verify formatting compliance without modifying files — for example, before committing or in CI. Returns a list of files that would be changed by `mix format`. On failure, the `hint` field provides the exact command to auto-fix formatting. Does not modify any files.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project directory (defaults to cwd)"},
                    "container": {"type": "string", "description": "Docker container to run inside"}
                }
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::elixir::format_check(dir, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("mix_format_check", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // mix_deps (Elixir)
    server.register_tool(
        ToolDef {
            name: "mix_deps".to_string(),
            description: "List Elixir project dependencies with versions and status using `mix deps`. Use this to check dependency versions, find outdated or missing packages, or verify dependency resolution. Returns structured JSON with each dependency's name, version, requirement, and status (ok, missing, diverged, etc.). On failure, the `hint` field provides recovery steps such as running `mix deps.get`.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project directory (defaults to cwd)"},
                    "container": {"type": "string", "description": "Docker container to run inside"}
                }
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::elixir::deps(dir, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("mix_deps", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // npm_tools (Node.js)
    server.register_tool(
        ToolDef {
            name: "npm_tools".to_string(),
            description: "Run Node.js/npm toolchain commands with structured JSON output. Use this instead of running npm/npx via Bash — it parses output into structured errors and results, saving significant tokens. Supports test (jest/vitest/mocha), typecheck (tsc --noEmit), lint (eslint), format_check (prettier --check), deps (npm ls), build (npm run build), and audit (npm audit) subcommands. On failure, the `hint` field provides actionable recovery steps. Returns JSON with `success` (boolean), structured diagnostics, and `summary` fields.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["test", "typecheck", "lint", "format_check", "deps", "build", "audit"], "description": "NPM subcommand: 'test' runs test suite, 'typecheck' runs tsc --noEmit, 'lint' runs eslint, 'format_check' runs prettier --check, 'deps' lists dependencies, 'build' runs npm run build, 'audit' checks for vulnerabilities"},
                    "path": {"type": "string", "description": "Node.js project directory containing package.json (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments, e.g. '-- --watch' or '--filter=my-package'"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                },
                "required": ["command"]
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::npm::run(cmd, dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("npm_tools", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // python_tools
    server.register_tool(
        ToolDef {
            name: "python_tools".to_string(),
            description: "Run Python toolchain commands with structured JSON output. Use this instead of running pytest/ruff/mypy via Bash — it parses output into structured errors and results, saving significant tokens. Supports test (pytest), lint (ruff/flake8), format_check (ruff format --check / black --check), deps (pip list), and typecheck (mypy/pyright) subcommands. On failure, the `hint` field provides actionable recovery steps. Returns JSON with `success` (boolean), structured diagnostics, and `summary` fields.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["test", "lint", "format_check", "deps", "typecheck"], "description": "Python subcommand: 'test' runs pytest, 'lint' runs ruff/flake8, 'format_check' checks formatting with ruff/black, 'deps' lists installed packages, 'typecheck' runs mypy/pyright"},
                    "path": {"type": "string", "description": "Python project directory containing pyproject.toml, setup.py, or requirements.txt (defaults to current working directory)"},
                    "args": {"type": "string", "description": "Additional CLI arguments, e.g. '-k test_auth' or '--no-header' or '-x' (stop on first failure)"},
                    "container": {"type": "string", "description": "Docker container name to run inside (for projects using containerized builds)"}
                },
                "required": ["command"]
            }),
            tier: 2,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            let container = args.get("container").and_then(|v| v.as_str());
            let result = forge_devtools::python::run(cmd, dir, extra, container);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("python_tools", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // ── devtools: infrastructure tools ──────────────────────────────────

    // docker_status
    server.register_tool(
        ToolDef {
            name: "docker_status".to_string(),
            description: "Check Docker Compose service status, reporting each container's state (running, stopped, exited), ports, and health. Use this when you need to verify that dependent services (databases, caches, message queues) are running before running tests or starting an application. If services are stopped, the `hint` field provides the exact `docker compose up` command to restart them. Returns structured JSON — not raw docker output.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory containing docker-compose.yml or compose.yaml (defaults to current working directory)"}
                }
            }),
            tier: 3,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let result = forge_devtools::docker::status(dir);
            let raw_input_bytes = result.base.raw_input_bytes;
            let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            record_mcp_analytics("docker_status", raw_input_bytes, &json, dir);
            Ok(json)
        }),
    );

    // git_summary
    server.register_tool(
        ToolDef {
            name: "git_summary".to_string(),
            description: "Get structured git repository information as JSON. Use this instead of running `git status`, `git log`, or `git diff` via Bash — it returns parsed, structured data rather than raw text. The 'status' command categorizes files into staged, modified, untracked, and deleted groups. The 'log' command returns recent commits with hash, author, date, and message. The 'diff' command returns the working tree diff. On issues, the `hint` field provides recovery steps. Significantly more token-efficient than raw git output.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["status", "log", "diff"], "description": "Git subcommand: 'status' shows working tree state (staged/modified/untracked/deleted), 'log' shows recent commit history, 'diff' shows uncommitted changes"},
                    "path": {"type": "string", "description": "Git repository directory (defaults to current working directory)"},
                    "count": {"type": "integer", "description": "Number of log entries to return (default: 10, only used with 'log' command)"}
                },
                "required": ["command"]
            }),
            tier: 1,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match cmd {
                "status" => {
                    let result = forge_devtools::git::status(dir);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                "log" => {
                    let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
                    let result = forge_devtools::git::log(dir, count);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                "diff" => {
                    let result = forge_devtools::git::diff(dir);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                _ => Err(format!("Unknown git command: {cmd}")),
            }
        }),
    );

    // ci_cd (GitHub Actions)
    server.register_tool(
        ToolDef {
            name: "ci_cd".to_string(),
            description: "Check GitHub Actions CI pipeline status, fetch workflow run logs, and list recent runs — all as structured JSON. Use this to monitor CI after pushing, diagnose failing workflows, or check if a branch is green before merging. Requires the `gh` CLI to be installed and authenticated. The 'check' command shows the latest run status for a branch. The 'logs' command fetches full logs for a specific run ID. The 'list' command shows recent workflow runs. On failure, the `hint` field tells you how to get detailed logs or fix authentication.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["check", "logs", "list"], "description": "CI subcommand: 'check' shows latest run status for a ref, 'logs' fetches full output for a run_id, 'list' shows recent workflow runs"},
                    "owner_repo": {"type": "string", "description": "GitHub repository in owner/name format, e.g. 'bkearns/research'"},
                    "ref": {"type": "string", "description": "Branch name or git ref to filter by, e.g. 'main' or 'feature/auth'"},
                    "run_id": {"type": "string", "description": "GitHub Actions run ID (required for 'logs' command). Get this from 'list' or 'check' output."},
                    "limit": {"type": "integer", "description": "Number of workflow runs to return (default: 5, only used with 'list' command)"}
                },
                "required": ["command", "owner_repo"]
            }),
            tier: 3,
            ..Default::default()
        },
        std::sync::Arc::new(|args| {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let repo = args.get("owner_repo").and_then(|v| v.as_str()).ok_or("Missing: owner_repo")?;
            let git_ref = args.get("ref").and_then(|v| v.as_str());
            match cmd {
                "check" => {
                    let result = forge_devtools::ci::check(repo, git_ref);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                "logs" => {
                    let run_id = args.get("run_id").and_then(|v| v.as_str()).ok_or("Missing: run_id")?;
                    let result = forge_devtools::ci::logs(repo, run_id);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                "list" => {
                    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as u32;
                    let result = forge_devtools::ci::list(repo, git_ref, limit);
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
                }
                _ => Err(format!("Unknown ci command: {cmd}")),
            }
        }),
    );

    // ── version ───────────────────────────────────────────────────────────

    register_tool!(
        server,
        "version",
        "Return the installed frg version number. Use this to verify the forge installation or check compatibility. Returns JSON with a single 'version' field containing the semantic version string.",
        serde_json::json!({"type": "object", "properties": {}}),
        |_args| { Ok(serde_json::json!({"version": env!("CARGO_PKG_VERSION")}).to_string()) }
    );

    // ── stdin-based tools ────────────────────────────────────────────────

    // test_summary
    register_stdin_tool!(server, "test_summary",
        "Parse raw test runner output from any major framework (cargo test, pytest, jest/vitest, go test, mix test) into structured JSON with pass/fail/skip counts and per-failure details. Use this when you have raw test output from a Bash command and need to extract structured results — for example, after running tests in a Docker container or CI environment where the language-specific MCP tool isn't available. Pipe the raw stdout/stderr text into the 'input' parameter. Returns JSON with `passed`, `failed`, `skipped` counts and a `failures` array with test name, file, line, and error message for each failure. Prefer the language-specific tools (cargo, python_tools, mix_test, etc.) when possible — they run tests AND parse output in one step.",
        |args| {
            let input = args.get("input").and_then(|v| v.as_str()).ok_or("Missing: input")?;
            let summary = forge_test_summary::parser::parse(input).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())
        }
    );

    // log_distill
    register_tool!(
        server,
        "log_distill",
        "Extract actionable errors and warnings from verbose build logs, stripping noise and keeping only diagnostics with surrounding context lines. Use this when processing long build/compile/deploy logs where you need to find the actual errors without reading hundreds of lines of successful output. Pass the raw log text via the 'input' parameter. Returns structured JSON with each error/warning, its severity, the source line, and configurable context lines around it. Especially useful for CI logs, Docker build output, or any verbose process output.",
        stdin_schema_with(serde_json::json!({
            "context": {"type": "integer", "description": "Number of surrounding context lines to include before and after each error/warning (default: 2). Increase for more context around complex errors."}
        })),
        |args| {
            let input = args
                .get("input")
                .and_then(|v| v.as_str())
                .ok_or("Missing: input")?;
            let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let result = forge_log_distill::distiller::distill(input, context);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // diff_filter
    register_tool!(
        server,
        "diff_filter",
        "Filter and reduce git diff output by skipping noise files (lock files, generated code, vendor directories), collapsing large hunks that exceed a line threshold, and optionally returning stats only. Use this when a git diff is too large to process in full — for example, when reviewing a large PR or analyzing changes across many files. Pass raw `git diff` output via the 'input' parameter. Returns JSON with the filtered diff, counts of files kept/skipped, and hunks collapsed. Use stats_only=true to get just file-level change statistics without any diff content.",
        stdin_schema_with(serde_json::json!({
            "include": {"type": "string", "description": "Comma-separated glob patterns to include — only files matching at least one pattern are kept, e.g. '*.rs,*.toml' or 'src/**'"},
            "max_hunk_lines": {"type": "integer", "description": "Maximum lines per hunk before it's collapsed to a summary (default: 80). Lower values produce more aggressive filtering."},
            "stats_only": {"type": "boolean", "description": "When true, returns only file-level change statistics (insertions, deletions) with no diff content. Defaults to false."}
        })),
        |args| {
            let input = args
                .get("input")
                .and_then(|v| v.as_str())
                .ok_or("Missing: input")?;
            let include_patterns = args
                .get("include")
                .and_then(|v| v.as_str())
                .map(|inc| inc.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let max_hunk_lines = args
                .get("max_hunk_lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(80) as usize;
            let stats_only = args
                .get("stats_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let config = forge_diff_filter::filter::FilterConfig {
                max_hunk_lines,
                include_patterns,
                ..Default::default()
            };
            if stats_only {
                let result = forge_diff_filter::filter::stats_only(input, &config);
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
            } else {
                let result = forge_diff_filter::filter::filter_diff(input, &config);
                let output = serde_json::json!({
                    "diff": result.output,
                    "files_kept": result.files_kept,
                    "files_skipped": result.files_skipped,
                    "hunks_collapsed": result.hunks_collapsed,
                });
                serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
            }
        }
    );

    // lint_dedup
    register_stdin_tool!(
        server,
        "lint_dedup",
        "Deduplicate and group lint warnings by rule ID, collapsing repetitive instances into a count + representative example for each rule. Use this when lint output contains hundreds of similar warnings (e.g., 50 instances of 'unused import') and you need a concise summary rather than the full list. Pass raw lint output (from any linter: clippy, eslint, ruff, credo, etc.) via the 'input' parameter. Returns structured JSON grouped by rule with occurrence counts and example locations.",
        |args| {
            let input = args
                .get("input")
                .and_then(|v| v.as_str())
                .ok_or("Missing: input")?;
            let result = forge_lint_dedup::dedup::dedup(input);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // log_monitor
    register_tool!(
        server,
        "log_monitor",
        "Analyze log output for stalls (repeated lines indicating a hang), errors, resource warnings (OOM, disk full), and overall completion status. Use this when monitoring long-running processes like builds, deployments, or data pipelines — especially when you need to detect if a process has stalled or hit a resource limit. Pass the accumulated log text via the 'input' parameter. Returns structured JSON with a top-level `status` field (completed, failed, stalled, resource_warning, in_progress) and arrays of detected events with timestamps and context.",
        stdin_schema_with(serde_json::json!({
            "stall_threshold": {"type": "integer", "description": "Number of consecutive identical lines required to flag a stall (default: 5). Lower values detect stalls earlier."},
            "repeat_threshold": {"type": "integer", "description": "Minimum repetition count before a repeated line is reported (default: 3)"},
            "max_events": {"type": "integer", "description": "Maximum number of events to include in the response (default: 50). Prevents oversized responses for very noisy logs."}
        })),
        |args| {
            let input = args
                .get("input")
                .and_then(|v| v.as_str())
                .ok_or("Missing: input")?;
            let stall_threshold = args
                .get("stall_threshold")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            let repeat_threshold = args
                .get("repeat_threshold")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let max_events = args
                .get("max_events")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            let config = forge_log_monitor::monitor::MonitorConfig {
                stall_threshold,
                repeat_threshold,
                max_events,
            };
            let result = forge_log_monitor::monitor::analyze(input, &config);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // ── path-based tools ─────────────────────────────────────────────────

    // coverage_gate
    register_tool!(server, "coverage_gate",
        "Validate that test coverage meets baseline requirements and that high-complexity code has proportionally higher coverage. Enforces the complexity-coverage coupling rule: functions with cyclomatic complexity >= 15 require 90% coverage, and functions with CC >= 25 require a refactor plan. Use this as a CI quality gate after running tests with coverage, or to check coverage health before merging. Requires an LCOV-format coverage file. Returns structured JSON with `passed` (boolean), overall coverage percentage, a list of violations (files below baseline, high-CC functions with insufficient coverage), and specific recommendations.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "coverage_file": {"type": "string", "description": "Path to LCOV-format coverage file, e.g. 'coverage/lcov.info' or 'target/coverage/lcov.info'"},
                "source": {"type": "string", "description": "Root directory of the source code that was measured, e.g. 'src/' or 'lib/'"},
                "baseline": {"type": "number", "description": "Minimum acceptable overall coverage percentage (default: 80). Gate fails if coverage falls below this."},
                "high_cc_threshold": {"type": "integer", "description": "Cyclomatic complexity threshold above which elevated coverage is required (default: 15)"},
                "high_cc_coverage": {"type": "number", "description": "Coverage percentage required for functions exceeding high_cc_threshold (default: 90)"},
                "critical_cc_threshold": {"type": "integer", "description": "Cyclomatic complexity threshold above which a refactor plan is required regardless of coverage (default: 25)"}
            },
            "required": ["coverage_file", "source"]
        }),
        |args| {
            let coverage_file = args.get("coverage_file").and_then(|v| v.as_str())
                .ok_or("Missing required parameter: coverage_file")?;
            let source = args.get("source").and_then(|v| v.as_str())
                .ok_or("Missing required parameter: source")?;
            let baseline = args.get("baseline").and_then(|v| v.as_f64()).unwrap_or(80.0);
            let high_cc_threshold = args.get("high_cc_threshold").and_then(|v| v.as_u64()).unwrap_or(15) as usize;
            let high_cc_coverage = args.get("high_cc_coverage").and_then(|v| v.as_f64()).unwrap_or(90.0);
            let critical_cc_threshold = args.get("critical_cc_threshold").and_then(|v| v.as_u64()).unwrap_or(25) as usize;

            let lcov_content = std::fs::read_to_string(coverage_file)
                .map_err(|e| format!("Cannot read coverage file {}: {}", coverage_file, e))?;
            let cov_data = forge_coverage_gate::gate::parse_lcov(&lcov_content);
            let source_path = std::path::PathBuf::from(source);
            let config = forge_coverage_gate::gate::GateConfig {
                baseline_coverage: baseline,
                high_cc_threshold,
                high_cc_coverage,
                critical_cc_threshold,
            };
            let result = forge_coverage_gate::gate::check(&cov_data, &source_path, &config);
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // doc_coverage
    register_path_tool!(server, "doc_coverage",
        "Scan source files for public API documentation coverage, reporting undocumented functions, structs, classes, and modules. Use this to identify documentation gaps before a release, during code review, or as part of a CI documentation gate. Accepts a single file or directory (scans recursively, respects .gitignore). Returns structured JSON per file with total_public (count of public items), documented (count with doc comments), and a list of undocumented items with names and line numbers. Only scans files with public items — empty results are excluded.",
        |args| {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or("Missing required parameter: path")?;
            let path = std::path::PathBuf::from(path_str);
            let mut reports = Vec::new();

            if path.is_file() {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Cannot read {}: {}", path_str, e))?;
                let report = forge_doc_coverage::scanner::scan(path_str, &source);
                reports.push(report);
            } else if path.is_dir() {
                for entry in ignore::WalkBuilder::new(&path).build().flatten() {
                    if entry.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Ok(source) = std::fs::read_to_string(entry.path()) {
                            let report = forge_doc_coverage::scanner::scan(
                                &entry.path().display().to_string(),
                                &source,
                            );
                            if report.total_public > 0 {
                                reports.push(report);
                            }
                        }
                    }
                }
            }

            serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())
        }
    );

    // concurrency_scan
    register_tool!(server, "concurrency_scan",
        "Scan source code for concurrency and distributed systems patterns, identifying uses of locks, mutexes, channels, consensus protocols, replication strategies, transaction handling, and failure recovery mechanisms. Use this when auditing a codebase for concurrency correctness, reviewing distributed system implementations, or understanding how a project handles parallelism and fault tolerance. Returns a structured report grouping findings by category (synchronization, consensus, replication, transaction, failure) with file locations and pattern descriptions. Accepts a single file or directory.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File or directory to scan recursively for concurrency patterns"},
                "categories": {"type": "string", "description": "Comma-separated list of categories to scan for (default: all). Options: synchronization (locks, mutexes, channels), consensus (Paxos, Raft, leader election), replication (primary-replica, CRDT), transaction (2PC, saga, WAL), failure (circuit breaker, retry, timeout)"}
            },
            "required": ["path"]
        }),
        |args| {
            let path_str = args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing required parameter: path")?;
            let path = std::path::PathBuf::from(path_str);

            let config = if let Some(cats) = args.get("categories").and_then(|v| v.as_str()) {
                let selected: Vec<forge_concurrency_scan::scanner::Category> = cats
                    .split(',')
                    .filter_map(|c| match c.trim().to_lowercase().as_str() {
                        "synchronization" => Some(forge_concurrency_scan::scanner::Category::Synchronization),
                        "consensus" => Some(forge_concurrency_scan::scanner::Category::Consensus),
                        "replication" => Some(forge_concurrency_scan::scanner::Category::Replication),
                        "transaction" => Some(forge_concurrency_scan::scanner::Category::Transaction),
                        "failure" => Some(forge_concurrency_scan::scanner::Category::Failure),
                        _ => None,
                    })
                    .collect();
                forge_concurrency_scan::scanner::ScanConfig { categories: selected }
            } else {
                forge_concurrency_scan::scanner::ScanConfig::default()
            };

            let mut scans = Vec::new();
            if path.is_file() {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    scans.push(forge_concurrency_scan::scanner::scan(path_str, &source, &config));
                }
            } else if path.is_dir() {
                for entry in ignore::WalkBuilder::new(&path).build().flatten() {
                    if entry.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Ok(source) = std::fs::read_to_string(entry.path()) {
                            scans.push(forge_concurrency_scan::scanner::scan(
                                &entry.path().display().to_string(),
                                &source,
                                &config,
                            ));
                        }
                    }
                }
            }

            let report = forge_concurrency_scan::scanner::build_report(scans);
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    register_tool!(server, "materialization_scan",
        "Find likely unbounded materialization in disk/storage/query I/O paths: whole-file reads, query rows_or_empty/ALLOW FILTERING result materialization, collect() in read paths, growing Vecs, and map-of-Vec grouping. Use this to build a checklist for fixing OOM risks by streaming, paging, chunking, server-side aggregates, or bounded buffers.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File or directory to scan recursively (respects .gitignore)"},
                "include_tests": {"type": "boolean", "description": "Include tests and fixtures; defaults to false"},
                "max_findings": {"type": "integer", "description": "Maximum findings to return; defaults to 500"}
            },
            "required": ["path"]
        }),
        |args| {
            let path_str = args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing required parameter: path")?;
            let config = forge_materialization_scan::scanner::ScanConfig {
                include_tests: args.get("include_tests").and_then(|v| v.as_bool()).unwrap_or(false),
                max_findings: args.get("max_findings").and_then(|v| v.as_u64()).unwrap_or(500) as usize,
            };
            let report = forge_materialization_scan::scanner::scan_path(std::path::Path::new(path_str), &config);
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // dsm (Design Structure Matrix analysis)
    register_tool!(server, "dsm",
        "Analyze codebase architecture using Design Structure Matrix (DSM) methodology — extract module dependencies, detect dependency cycles, cluster tightly-coupled modules, compute coupling/cohesion metrics, and generate refactoring suggestions. Use this for architectural analysis, identifying problematic coupling, planning module extraction, or generating architecture documentation. The 'extract' command returns raw dependency edges. The 'analyze' command runs the full pipeline (extract → matrix → cycles → clustering → metrics → suggestions) and returns a comprehensive report. Supports multiple output formats (markdown, JSON, Mermaid diagrams, CSV). Works with Rust, Python, Go, TypeScript, Elixir, Java, C/C++, and cross-language FFI/IPC.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "enum": ["extract", "analyze"], "description": "DSM subcommand: 'extract' returns raw dependency edges as JSON (fast, for programmatic use), 'analyze' runs the full pipeline producing a comprehensive architectural report with cycles, clusters, metrics, and refactoring suggestions"},
                "path": {"type": "string", "description": "Project root directory to analyze (defaults to current working directory). Scans recursively, respects .gitignore."},
                "level": {"type": "string", "enum": ["summary", "full"], "description": "Granularity: 'summary' groups by module/file (default), 'full' includes individual functions and types"},
                "prefix": {"type": "string", "description": "Filter to only include elements whose path starts with this prefix, e.g. 'src/core' or 'lib/auth'"},
                "format": {"type": "string", "enum": ["markdown", "json", "mermaid", "csv"], "description": "Output format for 'analyze' command (default: markdown). 'mermaid' produces a dependency diagram, 'csv' is for spreadsheet import."},
                "cross_language": {"type": "boolean", "description": "When true, also detect cross-language dependencies via FFI calls, IPC patterns, and shared protobuf/thrift definitions. Defaults to false."}
            },
            "required": ["command"]
        }),
        |args| {
            use forge_dsm_analyze::extract::{ExtractConfig, GranularityLevel};
            use forge_dsm_analyze::extract::multi::MultiExtractor;

            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("Missing: command")?;
            let dir_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let dir = std::path::PathBuf::from(dir_str);
            let level = match args.get("level").and_then(|v| v.as_str()).unwrap_or("summary") {
                "full" => GranularityLevel::Full,
                _ => GranularityLevel::Summary,
            };
            let prefix = args.get("prefix").and_then(|v| v.as_str()).map(|s| s.to_string());
            let cross_language = args.get("cross_language").and_then(|v| v.as_bool()).unwrap_or(false);

            let config = ExtractConfig {
                level,
                prefix_filter: prefix,
                exclude_patterns: vec![],
                detect_cross_language: cross_language,
            };

            match cmd {
                "extract" => {
                    let extractor = MultiExtractor::new();
                    let edges = extractor.extract_all(&dir, &config).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&edges).map_err(|e| e.to_string())
                }
                "analyze" => {
                    use forge_dsm_analyze::cluster::{cluster, ClusterConfig};
                    use forge_dsm_analyze::cycles::find_cycles;
                    use forge_dsm_analyze::matrix::DsmMatrix;
                    use forge_dsm_analyze::metrics::compute_metrics;
                    use forge_dsm_analyze::partition::partition;
                    use forge_dsm_analyze::report::{render, DsmReport, OutputFormat};
                    use forge_dsm_analyze::suggest::generate_suggestions;

                    let extractor = MultiExtractor::new();
                    let edges = extractor.extract_all(&dir, &config).map_err(|e| e.to_string())?;
                    if edges.is_empty() {
                        return Err("No dependencies extracted. Check that the project has source files.".to_string());
                    }
                    let edge_count = edges.len();
                    let matrix = DsmMatrix::from_edges(&edges);
                    let cycles = find_cycles(&matrix);
                    let cluster_config = ClusterConfig {
                        max_iterations: 10000,
                        num_runs: 5,
                        seed: None,
                        pow_cc: 1.0,
                    };
                    let clusters = cluster(&matrix, &cluster_config);
                    let metrics = compute_metrics(&matrix, &cycles, &clusters);
                    let part = partition(&matrix);
                    let suggestions = generate_suggestions(&matrix, &cycles, &clusters, &metrics, &part, None);

                    let project_name = dir.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "project".to_string());

                    let report = DsmReport {
                        project_name,
                        element_count: matrix.size(),
                        edge_count,
                        metrics,
                        cycles,
                        clusters,
                        partition: part,
                        suggestions,
                        directed: None,
                    };

                    let fmt = match args.get("format").and_then(|v| v.as_str()).unwrap_or("markdown") {
                        "json" => OutputFormat::Json,
                        "mermaid" => OutputFormat::Mermaid,
                        "csv" => OutputFormat::Csv,
                        _ => OutputFormat::Markdown,
                    };
                    Ok(render(&report, &fmt))
                }
                _ => Err(format!("Unknown dsm command: {cmd}. Use 'extract' or 'analyze'.")),
            }
        }
    );

    // ingest — extract codebase structure and docs into entities + edges, load into ferrosa-memory
    register_tool!(server, "ingest",
        "Extract codebase structure (crates, modules, dependencies) and documentation (markdown files, sections) and **ingest it into ferrosa-memory** via the `ingest_entities` MCP tool. Returns a LoadReport carrying insert/update counts per entity and edge kind — NOT extraction counts. Requires either an explicit `mcp_bin` argument (stdio subprocess) or `[server] transport = \"http\"` in `~/.config/ferrosa-memory.toml` (HTTP endpoint). Fails loud if neither is available — this tool never silently returns extraction counts as if they were loaded. Use `dry_run: true` for extraction-only. Supports Rust, Elixir, and Markdown.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path":    {"type": "string",  "description": "Path to the codebase root (defaults to current working directory)"},
                "mcp_bin": {"type": "string",  "description": "Optional. Path to the ferrosa-memory MCP binary. When omitted, forge uses the HTTP endpoint from ~/.config/ferrosa-memory.toml."},
                "session": {"type": "string",  "description": "Session UUID (read from config if omitted)"},
                "tenant":  {"type": "string",  "description": "Tenant UUID (read from config if omitted)"},
                "dry_run": {"type": "boolean", "description": "If true, return the extracted IngestReport without writing to ferrosa-memory. Default: false."}
            }
        }),
        |args| {
            let dir_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let dir = std::path::PathBuf::from(dir_str);
            let report = forge_ingest::extractor::extract(&dir).map_err(|e| e.to_string())?;
            persist_or_report(report, &args, "ingest")
        }
    );

    // ingest_url — fetch a web page and extract knowledge graph entities + edges
    register_tool!(server, "ingest_url",
        "Fetch a web page and ingest its structure and key concepts into a knowledge graph. Creates entities for the page, its sections, and key concepts, with typed edges (contains, related_to, references). Persists to ferrosa-memory via the configured MCP transport when available; otherwise returns the IngestReport JSON. Use `dry_run: true` for extraction-only.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch and ingest"},
                "depth": {"type": "integer", "description": "Crawl depth: 0=single page (default), 1=follow same-domain links, 2=two levels. Max 20 pages."},
                "mcp_bin": {"type": "string", "description": "Optional. Path to the ferrosa-memory MCP binary. When omitted, forge uses the HTTP endpoint from ~/.config/ferrosa-memory.toml."},
                "session": {"type": "string", "description": "Session UUID (read from config if omitted)"},
                "tenant": {"type": "string", "description": "Tenant UUID (read from config if omitted)"},
                "dry_run": {"type": "boolean", "description": "If true, return the extracted IngestReport without writing to ferrosa-memory. Default: false."}
            },
            "required": ["url"]
        }),
        |args| {
            let url = args.get("url").and_then(|v| v.as_str()).ok_or("url is required")?;
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let report = forge_ingest::url::extract_url_with_depth(url, depth).map_err(|e| e.to_string())?;
            persist_or_report(report, &args, "ingest-url")
        }
    );

    // ingest_paper — extract knowledge from academic papers (arxiv, IEEE, ACM, DOI, PDF)
    register_tool!(server, "ingest_paper",
        "Ingest an academic paper into a knowledge graph. Accepts arxiv URLs, DOIs (doi:10.xxx), Semantic Scholar links, IEEE/ACM URLs, bioRxiv, PubMed IDs, or local PDF paths. Extracts title, authors, abstract, references, key concepts, and document structure. Cleanses untrusted paper text against prompt injection before persistence. Uses fmem smart_ingest for entities, then inserts typed edges (wrote, references, discusses, affiliated_with, contains) after remapping entity ids chosen by fmem. Use `dry_run: true` for extraction-only.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {"type": "string", "description": "URL, DOI (doi:10.xxx), arxiv URL, or local PDF path"},
                "mcp_bin": {"type": "string", "description": "Optional. Path to the ferrosa-memory MCP binary. When omitted, forge uses the HTTP endpoint from ~/.config/ferrosa-memory.toml."},
                "session": {"type": "string", "description": "Session UUID (read from config if omitted)"},
                "tenant": {"type": "string", "description": "Tenant UUID (read from config if omitted)"},
                "dry_run": {"type": "boolean", "description": "If true, return the extracted IngestReport without writing to ferrosa-memory. Default: false."}
            },
            "required": ["input"]
        }),
        |args| {
            let input = args.get("input").and_then(|v| v.as_str()).ok_or("input is required")?;
            let report = forge_ingest::paper::extract_paper(input).map_err(|e| e.to_string())?;
            persist_paper_or_report(report, &args)
        }
    );

    // ingest_corpus — parse corpus distillation markdown and load into ferrosa-memory
    register_tool!(server, "ingest_corpus",
        "Ingest corpus distillation markdown files (or a directory of them) into a knowledge graph. Creates L1 document entities, L2 summary entities, and L3 per-section entities with contains/related_to edges. IDs are deterministic UUID v5 so re-runs are idempotent. Persists to ferrosa-memory via the configured MCP transport. Use dry_run: true for extraction-only.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path":    {"type": "string",  "description": "Path to a corpus .md file or directory (recursive)"},
                "mcp_bin": {"type": "string",  "description": "Optional. Path to the ferrosa-memory MCP binary. When omitted, uses HTTP endpoint from ~/.config/ferrosa-memory.toml."},
                "session": {"type": "string",  "description": "Session UUID (read from config if omitted)"},
                "tenant":  {"type": "string",  "description": "Tenant UUID (read from config if omitted)"},
                "dry_run": {"type": "boolean", "description": "If true, return the extracted IngestReport without writing to ferrosa-memory. Default: false."}
            },
            "required": ["path"]
        }),
        |args| {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or("path is required")?;
            let path = std::path::Path::new(path_str);
            let report = forge_ingest::corpus::extract_corpus(path).map_err(|e| e.to_string())?;
            persist_or_report(report, &args, "ingest-corpus")
        }
    );

    // task_create — create a new task
    register_tool!(server, "task_create",
        "Create a new task in the forge task board. Returns the created Task as JSON. The task starts in 'triage' status. Use task_update to transition status.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "title":          {"type": "string",  "description": "Task title (required)"},
                "body":           {"type": "string",  "description": "Task description / body"},
                "assignee":       {"type": "string",  "description": "Assignee name"},
                "reviewer":       {"type": "string",  "description": "Reviewer name"},
                "priority":       {"type": "integer", "description": "Priority (default 50; higher = more urgent)"},
                "workspace_kind": {"type": "string",  "description": "Workspace kind (e.g. worktree, branch)"},
                "workspace_path": {"type": "string",  "description": "Workspace path (e.g. repo root)"},
                "metadata":       {"type": "string",  "description": "Metadata as a JSON string"},
                "parents":        {"type": "array",   "items": {"type": "string"}, "description": "Parent task IDs to link to"},
                "skills":         {"type": "array",   "items": {"type": "string"}, "description": "Related skill names"},
                "created_by":     {"type": "string",  "description": "Creator identifier (default: agent)"},
                "cql_host":       {"type": "string",  "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["title"]
        }),
        |args| {
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store = forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let req = forge_tasks::CreateTaskRequest {
                title: args.get("title").and_then(|v| v.as_str()).ok_or("title is required")?.to_string(),
                body: args.get("body").and_then(|v| v.as_str()).map(str::to_string),
                assignee: args.get("assignee").and_then(|v| v.as_str()).map(str::to_string),
                reviewer: args.get("reviewer").and_then(|v| v.as_str()).map(str::to_string),
                priority: args.get("priority").and_then(|v| v.as_i64()).map(|i| i as i32),
                workspace_kind: args.get("workspace_kind").and_then(|v| v.as_str()).map(str::to_string),
                workspace_path: args.get("workspace_path").and_then(|v| v.as_str()).map(str::to_string),
                metadata: args.get("metadata").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
                created_by: args.get("created_by").and_then(|v| v.as_str()).map(str::to_string),
                skills: args.get("skills").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                }),
                parents: args.get("parents").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                }),
            };
            let task = store.create_task(req).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&task).map_err(|e| e.to_string())
        }
    );

    // task_update
    register_tool!(server, "task_update",
        "Update fields of an existing task. Only provided fields are changed. Returns the updated Task as JSON.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id":      {"type": "string",  "description": "Task ID (e.g. t_1a2b3c4d)"},
                "status":       {"type": "string",  "description": "New status: triage, ready, in_progress, blocked, complete, archived"},
                "assignee":     {"type": "string",  "description": "New assignee"},
                "reviewer":     {"type": "string",  "description": "New reviewer"},
                "priority":     {"type": "integer", "description": "New priority"},
                "title":        {"type": "string",  "description": "New title"},
                "body":         {"type": "string",  "description": "New body"},
                "block_reason": {"type": "string",  "description": "Block reason (use with status=blocked)"},
                "result":       {"type": "string",  "description": "Result summary"},
                "summary":      {"type": "string",  "description": "Task summary"},
                "cql_host":     {"type": "string",  "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["task_id"]
        }),
        |args| {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or("task_id is required")?;
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store = forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let patch = forge_tasks::UpdateTaskPatch {
                status: args.get("status").and_then(|v| v.as_str()).map(str::to_string),
                assignee: args.get("assignee").and_then(|v| v.as_str()).map(str::to_string),
                reviewer: args.get("reviewer").and_then(|v| v.as_str()).map(str::to_string),
                priority: args.get("priority").and_then(|v| v.as_i64()).map(|i| i as i32),
                title: args.get("title").and_then(|v| v.as_str()).map(str::to_string),
                body: args.get("body").and_then(|v| v.as_str()).map(str::to_string),
                block_reason: args.get("block_reason").and_then(|v| v.as_str()).map(str::to_string),
                result: args.get("result").and_then(|v| v.as_str()).map(str::to_string),
                summary: args.get("summary").and_then(|v| v.as_str()).map(str::to_string),
            };
            let task = store.update_task(task_id, patch).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&task).map_err(|e| e.to_string())
        }
    );

    // task_get
    register_tool!(
        server,
        "task_get",
        "Get a task by ID, including its parent/child links and recent comments.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id":  {"type": "string", "description": "Task ID (e.g. t_1a2b3c4d)"},
                "cql_host": {"type": "string", "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["task_id"]
        }),
        |args| {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or("task_id is required")?;
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store =
                forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let task = store.get_task(task_id).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&task).map_err(|e| e.to_string())
        }
    );

    // task_list
    register_tool!(
        server,
        "task_list",
        "List tasks with optional filtering by status, assignee, and priority range.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "status":       {"type": "string",  "description": "Filter by status: triage, ready, in_progress, blocked, complete, archived"},
                "assignee":     {"type": "string",  "description": "Filter by assignee name"},
                "priority_gte": {"type": "integer", "description": "Minimum priority (inclusive)"},
                "priority_lte": {"type": "integer", "description": "Maximum priority (inclusive)"},
                "limit":        {"type": "integer", "description": "Max results (default 50)"},
                "cql_host":     {"type": "string",  "description": "CQL host:port (default: 127.0.0.1:9042)"}
            }
        }),
        |args| {
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store =
                forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let filter = forge_tasks::TaskFilter {
                status: args
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                assignee: args
                    .get("assignee")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                priority_gte: args
                    .get("priority_gte")
                    .and_then(|v| v.as_i64())
                    .map(|i| i as i32),
                priority_lte: args
                    .get("priority_lte")
                    .and_then(|v| v.as_i64())
                    .map(|i| i as i32),
                limit: args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|i| i as usize),
            };
            let tasks = store.list_tasks(filter).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&tasks).map_err(|e| e.to_string())
        }
    );

    // task_link
    register_tool!(
        server,
        "task_link",
        "Create a parent→child relationship between two tasks.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "parent_id": {"type": "string", "description": "Parent task ID"},
                "child_id":  {"type": "string", "description": "Child task ID"},
                "cql_host":  {"type": "string", "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["parent_id", "child_id"]
        }),
        |args| {
            let parent_id = args
                .get("parent_id")
                .and_then(|v| v.as_str())
                .ok_or("parent_id is required")?;
            let child_id = args
                .get("child_id")
                .and_then(|v| v.as_str())
                .ok_or("child_id is required")?;
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store =
                forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            store
                .link_tasks(parent_id, child_id, "child")
                .map_err(|e| e.to_string())?;
            Ok(
                serde_json::json!({"ok": true, "parent_id": parent_id, "child_id": child_id})
                    .to_string(),
            )
        }
    );

    // task_unlink
    register_tool!(
        server,
        "task_unlink",
        "Remove the parent→child relationship between two tasks.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "parent_id": {"type": "string", "description": "Parent task ID"},
                "child_id":  {"type": "string", "description": "Child task ID"},
                "cql_host":  {"type": "string", "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["parent_id", "child_id"]
        }),
        |args| {
            let parent_id = args
                .get("parent_id")
                .and_then(|v| v.as_str())
                .ok_or("parent_id is required")?;
            let child_id = args
                .get("child_id")
                .and_then(|v| v.as_str())
                .ok_or("child_id is required")?;
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store =
                forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            store
                .unlink_tasks(parent_id, child_id)
                .map_err(|e| e.to_string())?;
            Ok(
                serde_json::json!({"ok": true, "parent_id": parent_id, "child_id": child_id})
                    .to_string(),
            )
        }
    );

    // task_comment
    register_tool!(
        server,
        "task_comment",
        "Add a comment to a task.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id":  {"type": "string", "description": "Task ID"},
                "author":   {"type": "string", "description": "Comment author (default: agent)"},
                "body":     {"type": "string", "description": "Comment text"},
                "cql_host": {"type": "string", "description": "CQL host:port (default: 127.0.0.1:9042)"}
            },
            "required": ["task_id", "body"]
        }),
        |args| {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or("task_id is required")?;
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or("body is required")?;
            let author = args
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store =
                forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let comment = store
                .add_comment(task_id, author, body)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&comment).map_err(|e| e.to_string())
        }
    );

    // task_board
    register_tool!(server, "task_board",
        "Return the kanban board with tasks grouped into columns: triage, ready, in_progress, blocked, complete.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "cql_host": {"type": "string", "description": "CQL host:port (default: 127.0.0.1:9042)"}
            }
        }),
        |args| {
            let cql_host =
                forge_tasks::resolve_cql_host(args.get("cql_host").and_then(|v| v.as_str()));
            let store = forge_tasks::TaskStore::connect(&cql_host, None).map_err(|e| e.to_string())?;
            let board = store.board().map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&board).map_err(|e| e.to_string())
        }
    );

    // fmem_skill_ingest — seed/refresh the ferrosa-memory skill catalog from a
    // research/skills tree. Runs the full four-phase orchestrator (taxonomy →
    // ingest → re-pass → verify) and returns the JSON summary.
    register_tool!(server, "fmem_skill_ingest",
        "Bulk ingest a research/skills SKILL.md catalog into ferrosa-memory. Four-phase pipeline: (A) seed tag taxonomy from tag-hierarchy.yaml, (B) call ingest_skill for every SKILL.md, (C) re-pass for any REQUIRES edges fmem skipped on first pass, (D) verify every skill via verify_skill and fail closed if any tag/prerequisite edge is missing. Idempotent via content_hash. Returns a JSON summary with per-phase buckets and an exit_code (0 clean, 1 parse, 2 transport, 3 precondition, 4 verification failure).",
        serde_json::json!({
            "type": "object",
            "properties": {
                "root": {"type": "string", "description": "Absolute path to the skill catalog root directory"},
                "filter": {"type": "string", "description": "Only ingest skills whose name matches this glob (e.g. 'tdd', 'try-*')"},
                "dry_run": {"type": "boolean", "description": "Parse and validate without calling fmem (default: false)"},
                "session": {"type": "string", "description": "Session UUID override (default: fmem-configured default)"},
                "force": {"type": "boolean", "description": "Re-ingest even when content_hash matches (default: false)"},
                "server": {"type": "string", "description": "Space-separated command that launches fmem (default: 'fmem --mcp')"},
                "verbose": {"type": "boolean", "description": "Include per-skill diagnostics in the summary (default: false)"}
            },
            "required": ["root"]
        }),
        |args| {
            let root = args.get("root").and_then(|v| v.as_str())
                .ok_or("root is required")?;
            let filter = args.get("filter").and_then(|v| v.as_str()).map(str::to_string);
            let dry_run = args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
            let session = args.get("session").and_then(|v| v.as_str()).map(str::to_string);
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let server = args.get("server").and_then(|v| v.as_str()).map(str::to_string);
            let verbose = args.get("verbose").and_then(|v| v.as_bool()).unwrap_or(false);

            let result = fmem_skill_ingest::run_as_mcp_tool(
                std::path::PathBuf::from(root),
                filter,
                dry_run,
                session,
                force,
                server,
                verbose,
            ).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
    );

    // mermaid_validate — lint Mermaid diagram syntax before writing it to a file
    register_stdin_tool!(server, "mermaid_validate",
        "Validate Mermaid diagram syntax. Use before writing a Mermaid diagram to a spec, README, or architecture doc — catches unknown diagram types, unbalanced brackets, bad edge syntax, undeclared sequence participants, and unterminated lines. Returns structured errors with line numbers. Input is the raw diagram text (no ```mermaid fence). Pure syntax check; does not render.",
        |args| {
            let input = args.get("input").and_then(|v| v.as_str()).unwrap_or("");
            let report = forge_mermaid_validate::validate(input);
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // checklist_state — persistent workflow checklist store for multi-session skills
    register_tool!(server, "checklist_state",
        "Persistent workflow checklist store. Use to save and resume multi-step workflow state across sessions (blueprint phases, compile-project task packets, performance-tuning methodology). Modes: create, create_dag, list, show, validate, ready, claim, set, note, release, delete. State is stored as JSON files under .forge/checklists/ in the project root. Call with mode=list to discover existing checklists.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["create", "create_dag", "list", "show", "validate", "ready", "claim", "set", "note", "release", "delete"], "description": "Operation to perform"},
                "name": {"type": "string", "description": "Checklist name (required for all modes except 'list')"},
                "titles": {"type": "array", "items": {"type": "string"}, "description": "Item titles for 'create'"},
                "items": {"type": "array", "description": "Rich checklist items for 'create_dag'"},
                "item_id": {"type": "string", "description": "Target item id for 'set', 'note', and 'release'"},
                "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "blocked"], "description": "New status for 'set'"},
                "text": {"type": "string", "description": "Note text for 'note'"},
                "agent_id": {"type": "string", "description": "Agent identifier for 'claim' and optional owner check for 'release'"},
                "limit": {"type": "integer", "description": "Maximum ready/claim items"},
                "lease_minutes": {"type": "integer", "description": "Claim lease duration in minutes (default: 60)"},
                "include_expired_leases": {"type": "boolean", "description": "Treat expired in-progress leases as ready/reclaimable"}
            },
            "required": ["mode"]
        }),
        |args| {
            let dir = std::env::current_dir().map_err(|e| e.to_string())?;
            let mode = args.get("mode").and_then(|v| v.as_str()).ok_or("mode is required")?;
            let name = || args.get("name").and_then(|v| v.as_str()).ok_or("name is required".to_string());
            match mode {
                "create" => {
                    let n = name()?;
                    let titles: Vec<String> = args.get("titles")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    let cl = forge_checklist_state::create(&dir, n, &titles).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "create_dag" => {
                    let n = name()?;
                    let items_value = args.get("items").cloned().ok_or("items is required")?;
                    let items: Vec<forge_checklist_state::ChecklistItem> = serde_json::from_value(items_value).map_err(|e| e.to_string())?;
                    let cl = forge_checklist_state::create_dag_from_items(&dir, n, items).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "list" => {
                    let names = forge_checklist_state::list(&dir).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&names).map_err(|e| e.to_string())
                }
                "show" => {
                    let n = name()?;
                    let cl = forge_checklist_state::show(&dir, n).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "validate" => {
                    let n = name()?;
                    let report = forge_checklist_state::validate(&dir, n).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
                }
                "ready" => {
                    let n = name()?;
                    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
                    let include_expired = args.get("include_expired_leases").and_then(|v| v.as_bool()).unwrap_or(false);
                    let report = forge_checklist_state::ready(&dir, n, limit, include_expired).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
                }
                "claim" => {
                    let n = name()?;
                    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).ok_or("agent_id is required")?;
                    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                    let lease_minutes = args.get("lease_minutes").and_then(|v| v.as_i64()).unwrap_or(60);
                    let include_expired = args.get("include_expired_leases").and_then(|v| v.as_bool()).unwrap_or(false);
                    let report = forge_checklist_state::claim(&dir, n, agent_id, limit, lease_minutes, include_expired).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
                }
                "set" => {
                    let n = name()?;
                    let item_id = args.get("item_id").and_then(|v| v.as_str()).ok_or("item_id is required")?;
                    let status = args.get("status").and_then(|v| v.as_str()).ok_or("status is required")?;
                    let st = forge_checklist_state::ItemStatus::parse(status).map_err(|e| e.to_string())?;
                    let cl = forge_checklist_state::set(&dir, n, item_id, st).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "note" => {
                    let n = name()?;
                    let item_id = args.get("item_id").and_then(|v| v.as_str()).ok_or("item_id is required")?;
                    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let cl = forge_checklist_state::note(&dir, n, item_id, text).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "release" => {
                    let n = name()?;
                    let item_id = args.get("item_id").and_then(|v| v.as_str()).ok_or("item_id is required")?;
                    let agent_id = args.get("agent_id").and_then(|v| v.as_str());
                    let cl = forge_checklist_state::release(&dir, n, item_id, agent_id).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&cl).map_err(|e| e.to_string())
                }
                "delete" => {
                    let n = name()?;
                    forge_checklist_state::delete(&dir, n).map_err(|e| e.to_string())?;
                    Ok(format!("{{\"deleted\":\"{n}\"}}"))
                }
                _ => Err(format!("unknown mode: {mode}")),
            }
        }
    );

    // todo_extract — inventory TODO/FIXME/HACK comments with git blame
    register_tool!(server, "todo_extract",
        "Extract a structured TODO/FIXME/HACK/XXX/BUG inventory from a source tree, optionally attributed via git blame. Use for debt triage, code-audit, refactor planning. Returns per-finding kind, file, line, text, author, commit, age in days, plus aggregate staleness buckets. Respects .gitignore. Prefer this over grep TODO -r when doing any debt-related analysis.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory to scan (default: cwd)"},
                "blame": {"type": "boolean", "description": "Attach git blame author and SHA (default: true)"},
                "kinds": {"type": "string", "description": "Comma-separated subset of TODO,FIXME,HACK,XXX,BUG,NOTE,OPTIMIZE,DEPRECATED"}
            }
        }),
        |args| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let blame = args.get("blame").and_then(|v| v.as_bool()).unwrap_or(true);
            let kinds: Vec<String> = args.get("kinds")
                .and_then(|v| v.as_str())
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default();
            let opts = forge_todo_extract::Options { blame, kinds };
            let report = forge_todo_extract::extract(std::path::Path::new(path), &opts).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // secret_scan — scan for leaked API keys, credentials, private keys
    register_path_tool!(server, "secret_scan",
        "Scan a directory for leaked API keys, AWS/GCP credentials, GitHub/Slack/Stripe tokens, JWTs, private key headers, and password assignments. Use before committing config changes, during secure-review, or as part of pipeline-defense. Returns masked snippets (never full plaintext) with severity per finding. Respects .gitignore, skips binary files. Prefer this over hand-rolled grep patterns.",
        |args| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let opts = forge_secret_scan::Options { min_entropy: None, include_entropy: false };
            let report = forge_secret_scan::scan(std::path::Path::new(path), &opts).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // deps_audit — parse dependency lockfiles and flag known-vulnerable versions
    register_tool!(server, "deps_audit",
        "Audit dependency lockfiles for known-vulnerable versions. Parses Cargo.lock, package-lock.json, mix.lock, go.sum, requirements.txt, Pipfile.lock, poetry.lock, Gemfile.lock. Uses an embedded vulnerability database (offline). Flags supply-chain attacks (node-ipc, colors sabotage, ua-parser-js, event-stream), Log4Shell, and other high-severity advisories. Use as a first-pass supply-chain check in pipeline-defense or before review of a dependency bump.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Project directory to audit (default: cwd)"},
                "min_severity": {"type": "string", "enum": ["low","medium","high","critical"], "description": "Minimum severity to report (default: medium)"}
            }
        }),
        |args| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let min_sev = args.get("min_severity").and_then(|v| v.as_str()).unwrap_or("medium");
            let sev = forge_deps_audit::Severity::parse(min_sev).map_err(|e| e.to_string())?;
            let opts = forge_deps_audit::Options { offline: true, min_severity: sev };
            let report = forge_deps_audit::audit(std::path::Path::new(path), &opts).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // threat_scan — STRIDE pattern scan
    register_tool!(server, "threat_scan",
        "Scan source code for STRIDE attack patterns: spoofing (missing auth, unverified JWT), tampering (SQL concatenation, eval, shell=True), repudiation (unaudited mutations), information_disclosure (stack traces in responses, hardcoded tokens), denial_of_service (unbounded loops, catastrophic regex, missing rate limits), elevation_of_privilege (string role checks, pickle.loads). Returns per-finding category, severity, confidence, file, line, recommendation. Use as a first-pass target list before threat-model or secure-review.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or directories to scan"},
                "categories": {"type": "string", "description": "Comma-separated: spoofing,tampering,repudiation,info_disclosure,dos,elevation"},
                "min_confidence": {"type": "string", "enum": ["low","medium","high"], "description": "Minimum confidence (default: medium)"}
            },
            "required": ["paths"]
        }),
        |args| {
            let paths: Vec<std::path::PathBuf> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(std::path::PathBuf::from)).collect())
                .unwrap_or_default();
            if paths.is_empty() {
                return Err("paths is required and must be non-empty".to_string());
            }
            let cats: Vec<forge_threat_scan::Category> = args.get("categories")
                .and_then(|v| v.as_str())
                .map(|s| s.split(',').filter_map(|t| forge_threat_scan::Category::parse(t.trim()).ok()).collect())
                .unwrap_or_default();
            let conf_s = args.get("min_confidence").and_then(|v| v.as_str()).unwrap_or("medium");
            let conf = forge_threat_scan::Confidence::parse(conf_s).map_err(|e| e.to_string())?;
            let opts = forge_threat_scan::Options { categories: cats, min_confidence: conf };
            let report = forge_threat_scan::scan(&paths, &opts).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // fail_loud_scan — AST-backed scan for fake success and swallowed failures
    register_tool!(server, "fail_loud_scan",
        "AST-backed scan for code that violates fail-loud behavior: swallowed errors, Err/exception paths converted into success/default data, runtime mock/fake/sample data leaks, and TODO/stub functions that return plausible fake values. Uses tree-sitter syntax trees and suppresses tests/fixtures/stories/examples by default to keep findings high-confidence. Use before release review or when auditing for applications that look complete while hiding unimplemented behavior.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or directories to scan"},
                "categories": {"type": "string", "description": "Comma-separated: swallowed_error,fake_success,mock_leak,placeholder_impl,optimistic_status"},
                "min_confidence": {"type": "string", "enum": ["medium","high"], "description": "Minimum confidence (default: high)"}
            },
            "required": ["paths"]
        }),
        |args| {
            let paths: Vec<std::path::PathBuf> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(std::path::PathBuf::from)).collect())
                .unwrap_or_default();
            if paths.is_empty() {
                return Err("paths is required and must be non-empty".to_string());
            }
            let cats: Vec<forge_fail_loud_scan::Category> = args.get("categories")
                .and_then(|v| v.as_str())
                .map(|s| s.split(',').filter_map(|t| forge_fail_loud_scan::Category::parse(t.trim()).ok()).collect())
                .unwrap_or_default();
            let conf_s = args.get("min_confidence").and_then(|v| v.as_str()).unwrap_or("high");
            let conf = forge_fail_loud_scan::Confidence::parse(conf_s).map_err(|e| e.to_string())?;
            let opts = forge_fail_loud_scan::Options { categories: cats, min_confidence: conf };
            let report = forge_fail_loud_scan::scan(&paths, &opts).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // schema_diff — detect breaking schema migrations
    register_tool!(server, "schema_diff",
        "Diff two SQL, CQL, or Cypher schemas to identify breaking migrations: dropped tables/columns, narrowed types, added NOT NULL, partition/clustering key changes, removed node labels or relationship types. Returns per-change severity (breaking/minor/patch) and a suggested semver bump. Use for migration review in sql-create, cql-create, graph-create.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "before": {"type": "string", "description": "Old schema source text"},
                "after": {"type": "string", "description": "New schema source text"},
                "dialect": {"type": "string", "enum": ["sql","cql","cypher"], "description": "Force dialect (default: auto-detect)"}
            },
            "required": ["before","after"]
        }),
        |args| {
            let before = args.get("before").and_then(|v| v.as_str()).ok_or("before is required")?;
            let after = args.get("after").and_then(|v| v.as_str()).ok_or("after is required")?;
            let dial = match args.get("dialect").and_then(|v| v.as_str()) {
                Some("sql") => Some(forge_schema_diff::Dialect::Sql),
                Some("cql") => Some(forge_schema_diff::Dialect::Cql),
                Some("cypher") => Some(forge_schema_diff::Dialect::Cypher),
                Some(other) => return Err(format!("unknown dialect: {other}")),
                None => None,
            };
            let report = forge_schema_diff::diff_schemas(before, after, dial).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // api_contract_diff — detect breaking API changes across revisions
    register_tool!(server, "api_contract_diff",
        "Diff the public API surface between two source files or directory trees. Reports added, removed, and signature-changed symbols (pub fn, pub struct, export, class, top-level def) with severity breaking/minor/patch and a suggested semver bump. Use for semver reasoning before a release or review. Supports Rust, TypeScript, Python, Go.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "before": {"type": "string", "description": "Path to the 'before' source file or directory"},
                "after": {"type": "string", "description": "Path to the 'after' source file or directory"},
                "lang": {"type": "string", "description": "Language hint (optional; auto-detected)"}
            },
            "required": ["before","after"]
        }),
        |args| {
            let before = args.get("before").and_then(|v| v.as_str()).ok_or("before is required")?;
            let after = args.get("after").and_then(|v| v.as_str()).ok_or("after is required")?;
            let before_path = std::path::Path::new(before);
            let after_path = std::path::Path::new(after);
            let lang = args.get("lang").and_then(|v| v.as_str());
            let report = if before_path.is_file() && after_path.is_file() {
                let before_src = std::fs::read_to_string(before_path).map_err(|e| e.to_string())?;
                let after_src = std::fs::read_to_string(after_path).map_err(|e| e.to_string())?;
                let filename = after_path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
                forge_api_diff::diff_sources(&before_src, &after_src, filename).map_err(|e| e.to_string())?
            } else {
                forge_api_diff::diff_trees(before_path, after_path, lang).map_err(|e| e.to_string())?
            };
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }
    );

    // tool_aliases — return alias map for resolving tool name mismatches
    server.register_tool(
        forge_mcp_server::ToolDef {
            name: "tool_aliases".to_string(),
            description: "Returns the tool alias map for resolving common tool name mismatches (e.g., 'Edit' → 'foundry.edit_file', 'Bash' → 'foundry.execute_command'). Use this when an agent invokes a tool by an alternate name to find the canonical Forge tool name.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            tier: 1,
            annotations: Some(forge_mcp_server::ToolAnnotations { read_only: true }),
        },
        std::sync::Arc::new(|_args| {
            let aliases = crate::aliases::get_alias_map();
            serde_json::to_string_pretty(&aliases).map_err(|e| e.to_string())
        }),
    );

    // glob — file discovery with structured stats (size, age, is_generated)
    server.register_tool(
        forge_mcp_server::ToolDef {
            name: "glob".to_string(),
            description: "Find files matching a glob pattern and return structured metadata (path, lines, bytes, modified time, is_generated). Apply filters by line count, byte size, and age. Respects .gitignore by default. A non-overridable secret-filename denylist (.env, *.pem, id_rsa, credentials.json, etc.) is always applied. Use this before digest/excerpt/read to discover which files are worth opening. Pattern must be relative unless 'allow_absolute' is true; '..' segments are always rejected.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern e.g. 'src/**/*.rs'. Relative to cwd unless allow_absolute=true."},
                    "min_lines": {"type": "integer", "description": "Skip files with fewer than N lines", "default": 0},
                    "max_lines": {"type": "integer", "description": "Skip files with more than N lines"},
                    "min_bytes": {"type": "integer", "description": "Skip files smaller than N bytes", "default": 0},
                    "max_bytes": {"type": "integer", "description": "Skip files larger than N bytes (default 1 GiB)"},
                    "modified_after": {"type": "string", "description": "Only files modified within this duration (e.g. '7d', '24h', '30m')"},
                    "modified_before": {"type": "string", "description": "Only files older than this duration"},
                    "exclude": {"type": "array", "items": {"type": "string"}, "description": "Additional exclude patterns (user-supplied; cannot override secret denylist)"},
                    "allow_absolute": {"type": "boolean", "description": "Permit absolute patterns like '/etc/**' (off by default)", "default": false},
                    "max_results": {"type": "integer", "description": "Cap result count (default 10000)", "default": 10000},
                    "max_depth": {"type": "integer", "description": "Maximum traversal depth", "default": 20},
                    "follow_links": {"type": "boolean", "description": "Follow symlinks (off by default)", "default": false},
                    "no_gitignore": {"type": "boolean", "description": "Disable .gitignore / .ignore respect", "default": false}
                },
                "required": ["pattern"]
            }),
            tier: 1,
            annotations: Some(forge_mcp_server::ToolAnnotations { read_only: true }),
        },
        std::sync::Arc::new(|args| {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or("Missing required parameter: pattern")?
                .to_string();
            let min_lines = args.get("min_lines").and_then(|v| v.as_u64()).unwrap_or(0);
            let max_lines = args
                .get("max_lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(u64::MAX);
            let min_bytes = args.get("min_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            let max_bytes = args
                .get("max_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024 * 1024 * 1024);
            let modified_after = args
                .get("modified_after")
                .and_then(|v| v.as_str())
                .map(String::from);
            let modified_before = args
                .get("modified_before")
                .and_then(|v| v.as_str())
                .map(String::from);
            let user_excludes: Vec<String> = args
                .get("exclude")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let allow_absolute = args
                .get("allow_absolute")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let max_results = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000) as usize;
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let follow_links = args
                .get("follow_links")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let respect_gitignore = !args
                .get("no_gitignore")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let cfg = crate::glob::GlobConfig {
                pattern,
                min_lines,
                max_lines,
                min_bytes,
                max_bytes,
                modified_after_secs: modified_after
                    .as_deref()
                    .map(crate::glob::parse_duration_public)
                    .transpose()
                    .map_err(|e: anyhow::Error| e.to_string())?,
                modified_before_secs: modified_before
                    .as_deref()
                    .map(crate::glob::parse_duration_public)
                    .transpose()
                    .map_err(|e: anyhow::Error| e.to_string())?,
                user_excludes,
                format: crate::glob::OutputFormat::Json,
                allow_absolute,
                max_results,
                max_depth,
                follow_links,
                respect_gitignore,
            };
            let out = crate::glob::collect(&cfg).map_err(|e| e.to_string())?;
            serde_json::to_string(&out).map_err(|e| e.to_string())
        }),
    );

    // list — return all available forge tools with names, descriptions, and tiers
    let tool_list: Vec<serde_json::Value> = server
        .tool_defs_visible(&detected_stacks)
        .iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "description": d.description,
                "tier": d.tier,
            })
        })
        .collect();
    let tool_list_arc = std::sync::Arc::new(tool_list);
    server.register_tool(
        forge_mcp_server::ToolDef {
            name: "list".to_string(),
            description: "Return the list of available forge MCP tools with their names, descriptions, and tier visibility. Use this when you need to discover or enumerate available forge tools, or when you want to check which tools are available for the current project. Returns a JSON array of tool objects with name, description, and tier (1=always visible, 2=stack-detected).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            tier: 1,
            annotations: Some(forge_mcp_server::ToolAnnotations { read_only: true }),
        },
        std::sync::Arc::new(move |_args| {
            serde_json::to_string_pretty(&*tool_list_arc).map_err(|e| e.to_string())
        }),
    );

    server.run()
}

/// Minimal parsed ferrosa-memory config.
struct FerrosaMemoryConfig {
    tenant_id: Option<String>,
    session_id: Option<String>,
    /// `"http"` or `"stdio"` — drives transport resolution in
    /// `resolve_transport_for_ingest`.
    transport: Option<String>,
    /// When true, the HTTP MCP endpoint is served over TLS and clients
    /// must use `https://` even though the transport name remains
    /// `"http"` in ferrosa-memory's config.
    require_tls: bool,
    http_bind_addr: Option<String>,
    http_port: Option<u16>,
    /// Client-side HTTP Basic credentials. Populated from the optional
    /// `[client]` section of `~/.config/ferrosa-memory.toml` (keys
    /// `http_username` and `http_password`). Plaintext in the client
    /// config is the standard cost for HTTP Basic — same trust class
    /// as `.netrc` / ssh key files. The server's auth file stores
    /// SHA-256 of the password and compares server-side.
    http_username: Option<String>,
    http_password: Option<String>,
    /// Per-call HTTP timeout override in milliseconds. Reads from
    /// `[client] http_timeout_ms`. `None` uses `DEFAULT_TIMEOUT_SECS`.
    http_timeout_ms: Option<u64>,
    /// Row-count cap per chunk for GraphLoader (entities + edges).
    /// Reads from `[client] max_rows_per_chunk`. `None` uses
    /// `GraphLoader::DEFAULT_MAX_ROWS_PER_CHUNK`.
    max_rows_per_chunk: Option<usize>,
}

impl FerrosaMemoryConfig {
    /// Resolve HTTP Basic credentials: env vars first (explicit override
    /// for scripted runs), then `[client]` section of the config file.
    fn resolve_http_auth(&self) -> Option<forge_fmem_client::HttpAuth> {
        if let Some(a) = forge_fmem_client::HttpAuth::from_env() {
            return Some(a);
        }
        let user = self.http_username.clone()?;
        let pass = self.http_password.clone()?;
        if user.is_empty() || pass.is_empty() {
            return None;
        }
        Some(forge_fmem_client::HttpAuth { user, pass })
    }
}

/// Read config from user's config directory if it exists.
fn ferrosa_memory_config() -> Option<FerrosaMemoryConfig> {
    let path = dirs::home_dir()?.join(".config/ferrosa-memory.toml");
    let content = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = content.parse().ok()?;
    let server = table.get("server")?.as_table()?;
    let client = table.get("client").and_then(|v| v.as_table());
    Some(FerrosaMemoryConfig {
        tenant_id: server
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        session_id: server
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        transport: server
            .get("transport")
            .and_then(|v| v.as_str())
            .map(String::from),
        require_tls: server
            .get("require_tls")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        http_bind_addr: server
            .get("bind_addr")
            .and_then(|v| v.as_str())
            .map(String::from),
        http_port: server
            .get("http_port")
            .and_then(|v| v.as_integer())
            .and_then(|n| u16::try_from(n).ok()),
        http_username: client
            .and_then(|c| c.get("http_username"))
            .and_then(|v| v.as_str())
            .map(String::from),
        http_password: client
            .and_then(|c| c.get("http_password"))
            .and_then(|v| v.as_str())
            .map(String::from),
        http_timeout_ms: client
            .and_then(|c| c.get("http_timeout_ms"))
            .and_then(|v| v.as_integer())
            .and_then(|n| u64::try_from(n).ok()),
        max_rows_per_chunk: client
            .and_then(|c| c.get("max_rows_per_chunk"))
            .and_then(|v| v.as_integer())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0),
    })
}

impl FerrosaMemoryConfig {
    /// Build a base HTTP URL from `bind_addr` + `http_port` when the
    /// server is configured for `transport = "http"`. `bind_addr` of
    /// `0.0.0.0` is rewritten to `127.0.0.1` for client use.
    fn http_base_url(&self) -> Option<String> {
        if self.transport.as_deref() != Some("http") {
            return None;
        }
        let port = self.http_port?;
        let host = self
            .http_bind_addr
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let host = if host == "0.0.0.0" {
            "127.0.0.1".to_string()
        } else {
            host
        };
        let scheme = if self.require_tls { "https" } else { "http" };
        Some(format!("{scheme}://{host}:{port}"))
    }
}

/// The resolved transport the CLI/MCP tool should use for this call,
/// with a short human-readable label for log/error messages.
enum ResolvedTransport {
    Http {
        transport: forge_fmem_client::HttpTransport,
        label: String,
    },
    Stdio {
        transport: forge_fmem_client::StdioTransport,
        label: String,
    },
}

impl ResolvedTransport {
    fn as_dyn(&self) -> &dyn forge_fmem_client::Transport {
        match self {
            ResolvedTransport::Http { transport, .. } => transport,
            ResolvedTransport::Stdio { transport, .. } => transport,
        }
    }
    fn label(&self) -> &str {
        match self {
            ResolvedTransport::Http { label, .. } => label,
            ResolvedTransport::Stdio { label, .. } => label,
        }
    }
}

/// Resolve a transport to use for an ingest/read operation.
///
/// Priority:
/// 1. Explicit `mcp_bin` argument → stdio subprocess
/// 2. Config `[server] transport = "http"` → HttpTransport against
///    `bind_addr:http_port`
///
/// Returns `Err` with a clear explanation when neither is available.
/// We DO NOT silently fall back to "extract-only" — a tool named
/// `ingest` that doesn't ingest is a footgun (a prior design mistake
/// this function exists to close).
fn resolve_transport_for_ingest(
    mcp_bin: Option<std::path::PathBuf>,
    config: &Option<FerrosaMemoryConfig>,
) -> anyhow::Result<ResolvedTransport> {
    if let Some(path) = mcp_bin {
        let transport = forge_fmem_client::StdioTransport::spawn(
            forge_fmem_client::transport::stdio::StdioConfig {
                command: vec![path.to_string_lossy().into_owned()],
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("failed to spawn MCP binary {}: {e}", path.display()))?;
        return Ok(ResolvedTransport::Stdio {
            transport,
            label: format!("stdio:{}", path.display()),
        });
    }

    if let Some(cfg) = config.as_ref() {
        if let Some(base_url) = cfg.http_base_url() {
            let auth = cfg.resolve_http_auth();
            let auth_label = if auth.is_some() { "authed" } else { "no-auth" };
            let timeout = cfg
                .http_timeout_ms
                .map(std::time::Duration::from_millis)
                .unwrap_or_else(|| {
                    std::time::Duration::from_secs(
                        forge_fmem_client::transport::http::DEFAULT_TIMEOUT_SECS,
                    )
                });
            let http_config = forge_fmem_client::HttpConfig {
                base_url: base_url.clone(),
                auth,
                timeout,
            };
            let transport = forge_fmem_client::HttpTransport::connect(http_config)
                .map_err(|e| anyhow::anyhow!("failed to connect to {base_url}: {e}"))?;
            return Ok(ResolvedTransport::Http {
                transport,
                label: format!("http:{base_url} ({auth_label})"),
            });
        }
    }

    anyhow::bail!(
        "no ferrosa-memory transport configured.\n\
         Provide one of:\n\
         - `--mcp-bin <path>` / `mcp_bin` arg to spawn a stdio subprocess\n\
         - `[server] transport = \"http\"` in ~/.config/ferrosa-memory.toml with `http_port` set\n\
         The forge ingest path refuses to return extraction counts as if they were load counts.",
    )
}

/// Push the extracted `report` through `ingest_entities` via
/// `GraphLoader`, returning the resulting `LoadReport`.
///
/// Persist an `IngestReport` to ferrosa-memory via MCP, or return the
/// report as JSON when `dry_run: true` is set in `args`.
///
/// Wraps `load_report_via_mcp` with the dry-run / arg-parsing
/// boilerplate the three ingest MCP tool callbacks (`ingest`,
/// `ingest_url`, `ingest_paper`) all share.  `label` is propagated to
/// the transport's eprintln banner so operators can tell which tool
/// produced the load report.  Consumes `report` because the MCP loader
/// takes ownership of the entity / edge buffers.
fn persist_or_report(
    report: forge_ingest::extractor::IngestReport,
    args: &serde_json::Value,
    label: &str,
) -> Result<String, String> {
    if args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return serde_json::to_string_pretty(&report).map_err(|e| e.to_string());
    }

    let mcp_bin = args
        .get("mcp_bin")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let session = args
        .get("session")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tenant = args
        .get("tenant")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let load_report = load_report_via_mcp(report, mcp_bin, session, tenant, label)
        // `{:#}` renders the full anyhow error chain (top: middle: root) —
        // `.to_string()` alone would drop everything below the first
        // `.with_context(...)` call, hiding the real server-side cause.
        .map_err(|e| format!("{e:#}"))?;
    serde_json::to_string_pretty(&load_report).map_err(|e| e.to_string())
}

/// Same as [`persist_or_report`], but academic papers use fmem `smart_ingest`
/// for entities before edge insertion. This avoids blind duplicate paper writes
/// while preserving the existing dry-run JSON report behaviour.
fn persist_paper_or_report(
    report: forge_ingest::extractor::IngestReport,
    args: &serde_json::Value,
) -> Result<String, String> {
    if args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return serde_json::to_string_pretty(&report).map_err(|e| e.to_string());
    }

    let mcp_bin = args
        .get("mcp_bin")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let session = args
        .get("session")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tenant = args
        .get("tenant")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let load_report = load_paper_report_via_mcp(report, mcp_bin, session, tenant)
        .map_err(|e| format!("{e:#}"))?;
    serde_json::to_string_pretty(&load_report).map_err(|e| e.to_string())
}

/// Transport precedence: explicit `mcp_bin` → `[server] transport =
/// "http"` from config → **error**. We never silently fall back to
/// extract-only — a tool named "ingest" that doesn't ingest is a
/// footgun.
fn load_report_via_mcp(
    report: forge_ingest::extractor::IngestReport,
    mcp_bin: Option<std::path::PathBuf>,
    cli_session: Option<String>,
    cli_tenant: Option<String>,
    label: &str,
) -> anyhow::Result<forge_ingest::graph_loader::LoadReport> {
    let fm_config = ferrosa_memory_config();
    let session_str = cli_session
        .or_else(|| fm_config.as_ref().and_then(|c| c.session_id.clone()))
        .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".to_string());
    let tenant_str = cli_tenant
        .or_else(|| fm_config.as_ref().and_then(|c| c.tenant_id.clone()))
        .ok_or_else(|| anyhow::anyhow!("tenant_id must be provided via --tenant or config file"))?;

    let transport = resolve_transport_for_ingest(mcp_bin, &fm_config)?;

    let entities_count = report.entities.len();
    let edges_count = report.edges.len();
    let mut loader = forge_ingest::graph_loader::GraphLoader::from_dyn(
        transport.as_dyn(),
        tenant_str.clone(),
        session_str.clone(),
    );
    if let Some(cap) = fm_config.as_ref().and_then(|c| c.max_rows_per_chunk) {
        loader = loader.with_max_rows_per_chunk(cap);
    }
    let batch = forge_ingest::graph_loader::GraphBatch {
        entities: report.entities,
        edges: report.edges,
    };
    eprintln!(
        "[frg {label}] transport={} tenant={tenant_str} session={session_str} \
         extracted_entities={entities_count} extracted_edges={edges_count}",
        transport.label()
    );
    let load_report = loader.load(batch)?;
    eprintln!(
        "[frg {label}] done: entities_inserted={} entities_updated={} edges_inserted={} \
         chunks={} duration_ms={}",
        load_report.entities_inserted,
        load_report.entities_updated,
        load_report.edges_inserted,
        load_report.chunks_submitted,
        load_report.duration_ms,
    );
    Ok(load_report)
}

fn load_paper_report_via_mcp(
    report: forge_ingest::extractor::IngestReport,
    mcp_bin: Option<std::path::PathBuf>,
    cli_session: Option<String>,
    cli_tenant: Option<String>,
) -> anyhow::Result<forge_ingest::graph_loader::LoadReport> {
    let fm_config = ferrosa_memory_config();
    let session_str = cli_session
        .or_else(|| fm_config.as_ref().and_then(|c| c.session_id.clone()))
        .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".to_string());
    let tenant_str = cli_tenant
        .or_else(|| fm_config.as_ref().and_then(|c| c.tenant_id.clone()))
        .ok_or_else(|| anyhow::anyhow!("tenant_id must be provided via --tenant or config file"))?;

    let transport = resolve_transport_for_ingest(mcp_bin, &fm_config)?;
    let entities_count = report.entities.len();
    let edges_count = report.edges.len();
    eprintln!(
        "[frg ingest-paper] transport={} tenant={tenant_str} session={session_str} \
         extracted_entities={entities_count} extracted_edges={edges_count} loader=smart_ingest",
        transport.label()
    );

    let loader = forge_ingest::smart_paper_loader::SmartPaperLoader::from_dyn(
        transport.as_dyn(),
        tenant_str.clone(),
        session_str.clone(),
    );
    let load_report = loader.load(report)?;
    eprintln!(
        "[frg ingest-paper] done: entities_inserted={} entities_updated={} \
         entities_skipped={} edges_inserted={} duration_ms={}",
        load_report.entities_inserted,
        load_report.entities_updated,
        load_report.entities_skipped,
        load_report.edges_inserted,
        load_report.duration_ms,
    );
    Ok(load_report)
}

/// Resolve an MCP binary for read-only / status queries: explicit flag →
/// `FERROSA_MEMORY_MCP_BIN` env → `which ferrosa-memory`. Returns `None`
/// when nothing resolves — callers should treat that as "skip the status
/// section" rather than fail.
fn resolve_mcp_bin_best_effort(explicit: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Some(p);
    }
    if let Ok(env) = std::env::var("FERROSA_MEMORY_MCP_BIN") {
        if !env.is_empty() {
            return Some(std::path::PathBuf::from(env));
        }
    }
    which::which("ferrosa-memory").ok()
}

/// Print the per-type entity breakdown for the session. Best-effort —
/// silently skips any step that fails so `frg context-check` never
/// becomes a blocker for the surrounding tooling (e.g. status-line hooks).
fn context_check_print(mcp_bin: Option<std::path::PathBuf>) {
    let fm_config = ferrosa_memory_config();
    let session = fm_config.as_ref().and_then(|c| c.session_id.clone());

    // ContextCheck is best-effort — we skip silently when neither
    // transport resolves. Unlike `ingest`, it's called on every
    // status-line tick; a hard failure here would be disruptive.
    let explicit = mcp_bin.or_else(|| resolve_mcp_bin_best_effort(None));
    let transport = match resolve_transport_for_ingest(explicit, &fm_config) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[context-check] skipping: {e}");
            return;
        }
    };

    let args = forge_fmem_client::CountEntitiesByTypeArgs {
        session_id: session,
    };
    let resp = match forge_fmem_client::count_entities_by_type(transport.as_dyn(), args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[context-check] skipping: count_entities_by_type failed: {e}");
            return;
        }
    };
    // Defense-in-depth: if the server disagrees with itself, log and skip
    // rather than print a potentially-wrong breakdown.
    if let Err(msg) = resp.assert_invariant() {
        eprintln!("[context-check] skipping: response invariant violation: {msg}");
        return;
    }
    if resp.total == 0 {
        return;
    }

    // Forge-specific 6-bucket classification — keeps parity with the 0.6.x
    // output. Unknown types roll into `code` so future types show up.
    let documents = resp.count_of_type("document");
    let sections = resp.count_of_type("section");
    let bugs_active = resp.count_of_type_state("bug", "active");
    let bugs_resolved = resp.count_of_type_state("bug", "resolved");
    let other_bug_states: u64 = resp
        .by_type_and_state
        .get("bug")
        .map(|inner| {
            inner
                .iter()
                .filter(|(s, _)| s.as_str() != "active" && s.as_str() != "resolved")
                .map(|(_, n)| *n)
                .sum()
        })
        .unwrap_or(0);
    let all_bugs = resp.count_of_type("bug");
    debug_assert_eq!(all_bugs, bugs_active + bugs_resolved + other_bug_states);
    let code = resp
        .total
        .saturating_sub(documents)
        .saturating_sub(sections)
        .saturating_sub(all_bugs);

    let mut parts: Vec<String> = Vec::new();
    if code > 0 {
        parts.push(format!("{code} code"));
    }
    if documents > 0 {
        parts.push(format!("{documents} docs"));
    }
    if sections > 0 {
        parts.push(format!("{sections} sections"));
    }
    if bugs_active > 0 {
        parts.push(format!("{bugs_active} bugs open"));
    }
    if bugs_resolved > 0 {
        parts.push(format!("{bugs_resolved} bugs resolved"));
    }
    if parts.is_empty() {
        println!("ferrosa-memory: {} entities ingested", resp.total);
    } else {
        println!(
            "ferrosa-memory: {} entities ingested ({})",
            resp.total,
            parts.join(", ")
        );
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // --mcp mode: run as MCP server over stdio
    if cli.mcp {
        return run_mcp_server();
    }

    let command = cli.command.ok_or_else(|| {
        anyhow::anyhow!("No command provided. Use --help for usage, or --mcp to run as MCP server.")
    })?;

    match command {
        Commands::TestSummary => {
            let input = forge_shared::read_stdin()?;
            let summary = forge_test_summary::parser::parse(&input)?;
            println!("{}", forge_shared::emit_json(&summary, cli.pretty)?);
        }

        Commands::LogDistill { context } => {
            let input = forge_shared::read_stdin()?;
            let result = forge_log_distill::distiller::distill(&input, context);
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::DiffFilter {
            include,
            max_hunk_lines,
            stats_only,
        } => {
            let input = forge_shared::read_stdin()?;
            let include_patterns = include
                .map(|inc| inc.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let config = forge_diff_filter::filter::FilterConfig {
                max_hunk_lines,
                include_patterns,
                ..Default::default()
            };
            if stats_only {
                let result = forge_diff_filter::filter::stats_only(&input, &config);
                println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
            } else {
                let result = forge_diff_filter::filter::filter_diff(&input, &config);
                print!("{}", result.output);
                eprintln!(
                    "{}",
                    forge_shared::emit_json(
                        &serde_json::json!({
                            "files_kept": result.files_kept,
                            "files_skipped": result.files_skipped,
                            "hunks_collapsed": result.hunks_collapsed,
                        }),
                        cli.pretty
                    )?
                );
            }
        }

        Commands::LintDedup => {
            let input = forge_shared::read_stdin()?;
            let result = forge_lint_dedup::dedup::dedup(&input);
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::LogMonitor {
            stall_threshold,
            repeat_threshold,
            max_events,
        } => {
            let input = forge_shared::read_stdin()?;
            let config = forge_log_monitor::monitor::MonitorConfig {
                stall_threshold,
                repeat_threshold,
                max_events,
            };
            let result = forge_log_monitor::monitor::analyze(&input, &config);
            let exit_code = match result.status {
                forge_log_monitor::monitor::LogStatus::Failed
                | forge_log_monitor::monitor::LogStatus::Stalled
                | forge_log_monitor::monitor::LogStatus::ResourceWarning => 1,
                _ => 0,
            };
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
            std::process::exit(exit_code);
        }

        Commands::CoverageGate {
            coverage,
            source,
            baseline,
            high_cc_threshold,
            high_cc_coverage,
            critical_cc_threshold,
        } => {
            let lcov_content = std::fs::read_to_string(&coverage)?;
            let cov_data = forge_coverage_gate::gate::parse_lcov(&lcov_content);
            let config = forge_coverage_gate::gate::GateConfig {
                baseline_coverage: baseline,
                high_cc_threshold,
                high_cc_coverage,
                critical_cc_threshold,
            };
            let result = forge_coverage_gate::gate::check(&cov_data, &source, &config);
            let exit_code = if result.passed { 0 } else { 1 };
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
            std::process::exit(exit_code);
        }

        Commands::SmellDetect {
            paths,
            max_lines,
            max_cc,
            max_nesting,
            max_params,
        } => {
            let config = forge_smell_detect::detector::DetectConfig {
                max_function_lines: max_lines,
                max_cc,
                max_nesting,
                max_params,
            };
            let mut reports = Vec::new();
            for path in &paths {
                if path.is_file() {
                    let source = std::fs::read_to_string(path)?;
                    let report = forge_smell_detect::detector::detect(
                        &path.display().to_string(),
                        &source,
                        &config,
                    );
                    if !report.smells.is_empty() {
                        reports.push(report);
                    }
                } else if path.is_dir() {
                    for entry in ignore::WalkBuilder::new(path).build().flatten() {
                        if entry.file_type().is_some_and(|ft| ft.is_file()) {
                            if let Ok(source) = std::fs::read_to_string(entry.path()) {
                                let report = forge_smell_detect::detector::detect(
                                    &entry.path().display().to_string(),
                                    &source,
                                    &config,
                                );
                                if !report.smells.is_empty() {
                                    reports.push(report);
                                }
                            }
                        }
                    }
                }
            }
            println!("{}", forge_shared::emit_json(&reports, cli.pretty)?);
        }

        Commands::DocCoverage { paths } => {
            let mut reports = Vec::new();
            for path in &paths {
                if path.is_file() {
                    let source = std::fs::read_to_string(path)?;
                    let report =
                        forge_doc_coverage::scanner::scan(&path.display().to_string(), &source);
                    reports.push(report);
                } else if path.is_dir() {
                    for entry in ignore::WalkBuilder::new(path).build().flatten() {
                        if entry.file_type().is_some_and(|ft| ft.is_file()) {
                            if let Ok(source) = std::fs::read_to_string(entry.path()) {
                                let report = forge_doc_coverage::scanner::scan(
                                    &entry.path().display().to_string(),
                                    &source,
                                );
                                if report.total_public > 0 {
                                    reports.push(report);
                                }
                            }
                        }
                    }
                }
            }
            println!("{}", forge_shared::emit_json(&reports, cli.pretty)?);
        }

        Commands::ProjectDetect { dir, summary } => {
            let detect_result = forge_project_detect::detector::detect(&dir);
            if summary {
                let sum = forge_project_detect::summary::summarize(&dir);
                let combined = serde_json::json!({
                    "detection": detect_result,
                    "summary": sum,
                });
                println!("{}", forge_shared::emit_json(&combined, cli.pretty)?);
            } else {
                println!("{}", forge_shared::emit_json(&detect_result, cli.pretty)?);
            }
        }

        Commands::Digest {
            paths,
            format,
            budget,
            since,
            grep,
        } => {
            // Step 1: If --since, get changed files from git and filter paths
            let effective_paths: Vec<PathBuf> = if let Some(ref git_ref) = since {
                let output = std::process::Command::new("git")
                    .args(["diff", "--name-only", git_ref])
                    .output()
                    .map_err(|e| anyhow::anyhow!("Failed to run git diff: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git diff --name-only {} failed: {}", git_ref, stderr);
                }
                let changed: std::collections::HashSet<PathBuf> =
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(PathBuf::from)
                        .collect();

                // Intersect: for each provided path, keep only changed files
                let mut result = Vec::new();
                for path in &paths {
                    if path.is_file() {
                        // Check if this file (or its canonicalized form) is in the changed set
                        if changed.contains(path)
                            || changed
                                .iter()
                                .any(|c| path.ends_with(c) || c.ends_with(path))
                        {
                            result.push(path.clone());
                        }
                    } else if path.is_dir() {
                        for entry in ignore::WalkBuilder::new(path).build().flatten() {
                            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                                let entry_path = entry.path().to_path_buf();
                                if changed
                                    .iter()
                                    .any(|c| entry_path.ends_with(c) || c.ends_with(&entry_path))
                                {
                                    result.push(entry_path);
                                }
                            }
                        }
                    }
                }
                result
            } else {
                paths.clone()
            };

            // Step 2: Collect digests normally
            let mut digests = Vec::new();
            for path in &effective_paths {
                if path.is_file() {
                    let source = std::fs::read_to_string(path)?;
                    let digest =
                        forge_digest::summarizer::summarize(&path.display().to_string(), &source);
                    digests.push(digest);
                } else if path.is_dir() {
                    for entry in ignore::WalkBuilder::new(path).build().flatten() {
                        if entry.file_type().is_some_and(|ft| ft.is_file()) {
                            if let Ok(source) = std::fs::read_to_string(entry.path()) {
                                let digest = forge_digest::summarizer::summarize(
                                    &entry.path().display().to_string(),
                                    &source,
                                );
                                if !digest.elements.is_empty() {
                                    digests.push(digest);
                                }
                            }
                        }
                    }
                }
            }

            // Step 3: If --grep, filter each digest and remove empty ones
            if let Some(ref pattern) = grep {
                let re = regex::Regex::new(pattern)
                    .map_err(|e| anyhow::anyhow!("Invalid grep pattern '{}': {}", pattern, e))?;
                digests = digests
                    .into_iter()
                    .map(|d| forge_digest::summarizer::filter_digest(&d, &re))
                    .filter(|d| !d.elements.is_empty())
                    .collect();
            }

            // Step 4: Format output
            if format == "outline" {
                if let Some(token_budget) = budget {
                    print!(
                        "{}",
                        forge_digest::summarizer::format_multi_outline_budgeted(
                            &digests,
                            token_budget
                        )
                    );
                } else {
                    print!(
                        "{}",
                        forge_digest::summarizer::format_multi_outline(&digests)
                    );
                }
            } else {
                println!("{}", forge_shared::emit_json(&digests, cli.pretty)?);
            }
        }

        // ── New commands inspired by RTK ──
        Commands::Run {
            tee,
            list_filters,
            args,
        } => {
            if list_filters {
                let registry = forge_shared::filters::FilterRegistry::load();
                let info = serde_json::json!({
                    "filters": registry.list(),
                    "fallback": &registry.fallback,
                    "source": format!("{:?}", registry.source),
                });
                println!("{}", forge_shared::emit_json(&info, cli.pretty)?);
                return Ok(());
            }

            let full_cmd = args.join(" ");
            let registry = forge_shared::filters::FilterRegistry::load();
            let project_dir = std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string());

            let (raw_output, exit_code) = run_command(&args)?;
            let input_bytes = raw_output.len();

            // Detect and apply the appropriate filter (with fallback)
            let fr = filter_output(&registry, &full_cmd, &raw_output, cli.pretty);
            let filter = fr.filter_name.clone();
            let filtered = fr.output;
            let filter_ok = fr.success;
            let filter_err = fr.error;
            let duration_ms = fr.duration_ms;
            let output_bytes = filtered.len();

            // Tee: save raw output on failure
            let tee_path = if tee && exit_code != 0 {
                forge_shared::tee::save_raw_output(&full_cmd, &raw_output).ok()
            } else {
                None
            };

            // Print filtered output
            println!("{}", filtered);

            // Show tee path if saved
            if let Some(path) = &tee_path {
                eprintln!("[raw output saved: {}]", path.display());
            }

            // Track token savings (both legacy and detailed)
            if let Ok(db_path) = forge_shared::tracking::default_db_path() {
                if let Ok(conn) = forge_shared::tracking::open_db(&db_path) {
                    let _ = forge_shared::tracking::record(
                        &conn,
                        &full_cmd,
                        &filter,
                        input_bytes,
                        output_bytes,
                        exit_code,
                    );
                    let _ = forge_shared::tracking::record_filter(
                        &conn,
                        &forge_shared::tracking::FilterRecord {
                            command: &full_cmd,
                            filter_name: &filter,
                            mode: forge_shared::tracking::InvocationMode::Run,
                            project_dir: project_dir.as_deref(),
                            input_bytes,
                            output_bytes,
                            duration_ms,
                            filter_success: filter_ok,
                            error_message: filter_err.as_deref(),
                            exit_code,
                            tool_version: env!("CARGO_PKG_VERSION"),
                        },
                    );
                }
            }

            // Preserve original exit code
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }

        Commands::Gain { json } => {
            let db_path = forge_shared::tracking::default_db_path()?;
            let conn = forge_shared::tracking::open_db(&db_path)?;
            let gains = forge_shared::tracking::query_gains(&conn)?;

            if json || !cli.pretty {
                // JSON mode (default for pipe compatibility)
                println!("{}", forge_shared::emit_json(&gains, cli.pretty)?);
            } else {
                // Human-readable report
                print!("{}", forge_shared::tracking::format_gains_report(&gains));
            }
        }

        Commands::Analytics { json } => {
            let db_path = forge_shared::tracking::default_db_path()?;
            let conn = forge_shared::tracking::open_db(&db_path)?;
            let analytics = forge_shared::tracking::query_filter_analytics(&conn)?;

            if json || !cli.pretty {
                println!("{}", forge_shared::emit_json(&analytics, cli.pretty)?);
            } else {
                println!("Filter Analytics");
                println!("{}", "=".repeat(60));
                println!("Total invocations: {}\n", analytics.total_invocations);

                if !analytics.by_filter.is_empty() {
                    println!(
                        "  {:<18} {:>6} {:>8} {:>10} {:>8} {:>5}",
                        "FILTER", "COUNT", "AVG_MS", "SAVED_TK", "RATIO", "FAIL"
                    );
                    for f in &analytics.by_filter {
                        println!(
                            "  {:<18} {:>6} {:>7.1}ms {:>10} {:>7.1}% {:>5}",
                            f.filter_name,
                            f.count,
                            f.avg_duration_ms,
                            f.total_saved_tokens,
                            (1.0 - f.avg_compression_ratio) * 100.0,
                            f.failure_count
                        );
                    }
                }

                if !analytics.by_mode.is_empty() {
                    println!("\nBy Invocation Mode:");
                    for m in &analytics.by_mode {
                        println!(
                            "  {:<10} {:>6} invocations, {:>10} tokens saved",
                            m.mode, m.count, m.total_saved_tokens
                        );
                    }
                }

                if !analytics.by_project.is_empty() {
                    println!("\nTop Projects:");
                    for p in &analytics.by_project {
                        println!(
                            "  {} ({} cmds, {} saved, top: {})",
                            p.project_dir, p.count, p.total_saved_tokens, p.top_filter
                        );
                    }
                }

                if !analytics.failures.is_empty() {
                    println!("\nRecent Failures:");
                    for f in &analytics.failures {
                        println!(
                            "  {} {} [{}]: {}",
                            f.timestamp, f.filter_name, f.command, f.error_message
                        );
                    }
                }
            }
        }

        Commands::ClearAnalytics => {
            let db_path = forge_shared::tracking::default_db_path()?;
            let conn = forge_shared::tracking::open_db(&db_path)?;
            let (filter_rows, command_rows) = forge_shared::tracking::clear_analytics(&conn)?;
            println!(
                "Cleared {} filter_log rows and {} command_log rows.",
                filter_rows, command_rows
            );
        }

        Commands::Init {
            global,
            uninstall,
            show,
        } => {
            let settings_path = if global {
                dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
                    .join(".claude")
                    .join("settings.json")
            } else {
                PathBuf::from(".claude").join("settings.json")
            };

            if show {
                if settings_path.exists() {
                    let content = std::fs::read_to_string(&settings_path)?;
                    let has_forge = content.contains("forge");

                    // Check if installed hook matches canonical form
                    let hook_status = if has_forge {
                        if content.contains(CANONICAL_HOOK_COMMAND) {
                            "current"
                        } else {
                            "outdated"
                        }
                    } else {
                        "not_installed"
                    };

                    let mut result = serde_json::json!({
                        "settings_path": settings_path.display().to_string(),
                        "forge_hooks_installed": has_forge,
                        "hook_status": hook_status,
                        "hook_schema_version": HOOK_SCHEMA_VERSION,
                    });

                    if hook_status == "outdated" {
                        result["message"] = serde_json::Value::String(
                            "Installed hook uses old format. Run `frg init --global` to upgrade."
                                .to_string(),
                        );
                    }

                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "{}",
                        serde_json::json!({
                            "settings_path": settings_path.display().to_string(),
                            "forge_hooks_installed": false,
                            "hook_status": "not_installed",
                            "note": "Settings file does not exist"
                        })
                    );
                }
                return Ok(());
            }

            if uninstall {
                if settings_path.exists() {
                    let content = std::fs::read_to_string(&settings_path)?;
                    if let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&content) {
                        // Remove forge hooks from all phases
                        if let Some(hooks) = settings.get_mut("hooks") {
                            for phase in ["PreToolUse", "PostToolUse"] {
                                if let Some(arr) =
                                    hooks.get_mut(phase).and_then(|v| v.as_array_mut())
                                {
                                    arr.retain(|h| !h.to_string().contains("forge"));
                                }
                            }
                        }
                        let out = serde_json::to_string_pretty(&settings)?;
                        std::fs::write(&settings_path, out)?;
                        println!("Removed forge hooks from {}", settings_path.display());
                    }
                } else {
                    println!("No settings file found at {}", settings_path.display());
                }
                return Ok(());
            }

            // Install hooks
            let hook_config = generate_hook_config();

            if settings_path.exists() {
                let content = std::fs::read_to_string(&settings_path)?;
                let mut settings: serde_json::Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));

                // Merge hooks for all phases
                if let Some(new_hooks) = hook_config.get("hooks") {
                    if settings.get("hooks").is_none() {
                        settings["hooks"] = serde_json::json!({});
                    }
                    for phase in ["PreToolUse", "PostToolUse"] {
                        if let Some(phase_hooks) = new_hooks.get(phase) {
                            settings["hooks"][phase] = phase_hooks.clone();
                        }
                    }
                }

                let out = serde_json::to_string_pretty(&settings)?;
                std::fs::write(&settings_path, out)?;
            } else {
                // Create directory and write new settings
                if let Some(parent) = settings_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let out = serde_json::to_string_pretty(&hook_config)?;
                std::fs::write(&settings_path, out)?;
            }

            let scope = if global { "global" } else { "project" };
            println!(
                "Installed forge hooks ({}) at {}",
                scope,
                settings_path.display()
            );
            println!("Restart Claude Code to activate hooks.");
        }

        Commands::Discover { dir } => {
            let project = forge_project_detect::detector::detect(&dir);

            #[derive(serde::Serialize)]
            struct Opportunity {
                command: String,
                filter: String,
                estimated_savings: String,
                example: String,
            }

            let mut opportunities = Vec::new();

            // Check for test runners
            let test_commands: Vec<(&str, &str)> = vec![
                ("Cargo.toml", "cargo test"),
                ("pyproject.toml", "pytest"),
                ("setup.py", "pytest"),
                ("package.json", "jest/vitest"),
                ("mix.exs", "mix test"),
                ("go.mod", "go test ./..."),
            ];

            for (marker, cmd) in &test_commands {
                if dir.join(marker).exists() {
                    opportunities.push(Opportunity {
                        command: cmd.to_string(),
                        filter: "test-summary".to_string(),
                        estimated_savings: "90-95%".to_string(),
                        example: format!("frg run {} 2>&1", cmd),
                    });
                }
            }

            // Check for linters
            let lint_commands: Vec<(&str, &str)> = vec![
                ("Cargo.toml", "cargo clippy"),
                ("pyproject.toml", "ruff check ."),
                (".eslintrc.json", "eslint src/"),
                (".eslintrc.js", "eslint src/"),
                ("biome.json", "biome check"),
            ];

            for (marker, cmd) in &lint_commands {
                if dir.join(marker).exists() {
                    opportunities.push(Opportunity {
                        command: cmd.to_string(),
                        filter: "lint-dedup".to_string(),
                        estimated_savings: "80-85%".to_string(),
                        example: format!("frg run {} 2>&1", cmd),
                    });
                }
            }

            // Git diff is always available
            if dir.join(".git").exists() {
                opportunities.push(Opportunity {
                    command: "git diff".to_string(),
                    filter: "diff-filter".to_string(),
                    estimated_savings: "40-80%".to_string(),
                    example: "git diff | frg diff-filter".to_string(),
                });
            }

            // Build commands
            let build_commands: Vec<(&str, &str)> = vec![
                ("Cargo.toml", "cargo build"),
                ("package.json", "npm run build"),
                ("Makefile", "make"),
            ];

            for (marker, cmd) in &build_commands {
                if dir.join(marker).exists() {
                    opportunities.push(Opportunity {
                        command: cmd.to_string(),
                        filter: "log-distill".to_string(),
                        estimated_savings: "60-90%".to_string(),
                        example: format!("frg run {} 2>&1", cmd),
                    });
                }
            }

            let result = serde_json::json!({
                "project": project,
                "opportunities": opportunities,
                "total_opportunities": opportunities.len(),
                "setup": "Run `frg init --global` to auto-compress all command output"
            });

            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::Excerpt { target, context } => {
            // Split on the LAST colon so paths like C:\foo\bar.rs:symbol work
            let (file_path, symbol) = match target.rfind(':') {
                Some(pos) if pos > 0 && pos < target.len() - 1 => {
                    (&target[..pos], &target[pos + 1..])
                }
                _ => {
                    eprintln!("Error: target must be FILE:SYMBOL (e.g. src/main.rs:process_data)");
                    std::process::exit(1);
                }
            };

            let source = std::fs::read_to_string(file_path)
                .map_err(|e| anyhow::anyhow!("Cannot read '{}': {}", file_path, e))?;

            match forge_digest::excerpt::extract_symbol(file_path, &source, symbol) {
                Some(result) => {
                    // Print with cat -n style line numbers
                    let lines: Vec<&str> = result.body.lines().collect();
                    let width = format!("{}", result.end_line).len();
                    for (i, line) in lines.iter().enumerate() {
                        let line_no = result.start_line + i;
                        println!("{:>width$}\t{}", line_no, line, width = width);
                    }
                    // Also add context lines before, if requested and available
                    let _ = context; // context is used for doc comments which are already included
                }
                None => {
                    eprintln!("Symbol '{}' not found in '{}'", symbol, file_path);
                    std::process::exit(1);
                }
            }
        }

        Commands::Lookup { symbol, dir } => {
            let result = forge_digest::lookup::lookup_symbol(&symbol, &dir)?;
            if cli.pretty {
                println!("{}", forge_digest::lookup::format_lookup(&result));
            } else {
                println!("{}", forge_shared::emit_json(&result, false)?);
            }
        }

        Commands::Context { session, clear } => {
            let session_id = session.unwrap_or_else(|| {
                std::env::var("CLAUDE_SESSION_ID")
                    .unwrap_or_else(|_| format!("pid-{}", std::process::id()))
            });

            let db_path = forge_shared::tracking::default_db_path()?;
            let conn = forge_shared::tracking::open_db(&db_path)?;

            if clear {
                forge_shared::tracking::clear_context(&conn, &session_id)?;
                println!("Cleared context for session: {}", session_id);
            } else {
                let entries = forge_shared::tracking::query_context(&conn, &session_id)?;
                if entries.is_empty() {
                    println!("No context entries for session: {}", session_id);
                } else {
                    println!("{:<40} {:<24} {:>10} TIMESTAMP", "FILE", "SYMBOL", "BYTES");
                    println!("{}", "-".repeat(90));
                    for e in &entries {
                        let symbol_display = e.symbol.as_deref().unwrap_or("-");
                        let range = match (e.start_line, e.end_line) {
                            (Some(s), Some(end)) => format!("{}:{}-{}", e.file_path, s, end),
                            (Some(s), None) => format!("{}:{}", e.file_path, s),
                            _ => e.file_path.clone(),
                        };
                        println!(
                            "{:<40} {:<24} {:>10} {}",
                            range, symbol_display, e.byte_count, e.timestamp
                        );
                    }
                    println!(
                        "\n{} entries, {} total bytes",
                        entries.len(),
                        entries.iter().map(|e| e.byte_count).sum::<usize>()
                    );
                }
            }
        }

        Commands::Dsm { action } => {
            handle_dsm(action, cli.pretty)?;
        }

        Commands::ConcurrencyScan { paths, categories } => {
            let config = if let Some(cats) = categories {
                let selected: Vec<forge_concurrency_scan::scanner::Category> = cats
                    .split(',')
                    .filter_map(|c| match c.trim().to_lowercase().as_str() {
                        "synchronization" => {
                            Some(forge_concurrency_scan::scanner::Category::Synchronization)
                        }
                        "consensus" => Some(forge_concurrency_scan::scanner::Category::Consensus),
                        "replication" => {
                            Some(forge_concurrency_scan::scanner::Category::Replication)
                        }
                        "transaction" => {
                            Some(forge_concurrency_scan::scanner::Category::Transaction)
                        }
                        "failure" => Some(forge_concurrency_scan::scanner::Category::Failure),
                        _ => None,
                    })
                    .collect();
                forge_concurrency_scan::scanner::ScanConfig {
                    categories: selected,
                }
            } else {
                forge_concurrency_scan::scanner::ScanConfig::default()
            };

            let mut scans = Vec::new();
            for path in &paths {
                if path.is_file() {
                    if let Ok(source) = std::fs::read_to_string(path) {
                        let file_scan = forge_concurrency_scan::scanner::scan(
                            &path.display().to_string(),
                            &source,
                            &config,
                        );
                        scans.push(file_scan);
                    }
                } else if path.is_dir() {
                    for entry in ignore::WalkBuilder::new(path).build().flatten() {
                        if entry.file_type().is_some_and(|ft| ft.is_file()) {
                            if let Ok(source) = std::fs::read_to_string(entry.path()) {
                                let file_scan = forge_concurrency_scan::scanner::scan(
                                    &entry.path().display().to_string(),
                                    &source,
                                    &config,
                                );
                                scans.push(file_scan);
                            }
                        }
                    }
                }
            }

            let report = forge_concurrency_scan::scanner::build_report(scans);
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::Outline { file } => {
            let source = std::fs::read_to_string(&file)?;
            let result = forge_outline::outline(&file.display().to_string(), &source);
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::MaterializationScan {
            paths,
            include_tests,
            max_findings,
        } => {
            let config = forge_materialization_scan::scanner::ScanConfig {
                include_tests,
                max_findings,
            };
            let mut combined = forge_materialization_scan::scanner::ScanReport {
                scanned_files: 0,
                finding_count: 0,
                findings: Vec::new(),
            };
            for path in paths {
                let mut report = forge_materialization_scan::scanner::scan_path(&path, &config);
                combined.scanned_files += report.scanned_files;
                combined.findings.append(&mut report.findings);
                if combined.findings.len() >= max_findings {
                    combined.findings.truncate(max_findings);
                    break;
                }
            }
            combined.finding_count = combined.findings.len();
            println!("{}", forge_shared::emit_json(&combined, cli.pretty)?);
        }

        Commands::DepTree { dir } => {
            let result = forge_dep_tree::build_dep_tree(&dir)?;
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::FormatFix { dir, check } => {
            let result = forge_format_fix::format_fix(&dir, check)?;
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::MergeCheck {
            source_branch,
            target_branch,
            strategy: _,
        } => {
            let dir = std::env::current_dir()?;
            let result =
                forge_merge_check::merge_check(&dir, &source_branch, target_branch.as_deref())?;
            println!("{}", forge_shared::emit_json(&result, cli.pretty)?);
        }

        Commands::ContextCheck { dir: _, mcp_bin } => {
            context_check_print(mcp_bin);
        }

        Commands::Ingest {
            dir,
            mcp_bin,
            session,
            tenant,
            dry_run,
        } => {
            let report = forge_ingest::extractor::extract(&dir)?;
            if dry_run {
                println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
            } else {
                let load_report = load_report_via_mcp(report, mcp_bin, session, tenant, "ingest")?;
                println!("{}", forge_shared::emit_json(&load_report, cli.pretty)?);
            }
        }
        Commands::IngestDescriptions {
            dir,
            desc_provider,
            desc_model,
            desc_endpoint,
            desc_timeout_ms,
            desc_include_private,
            desc_min_confidence,
            desc_concurrency,
            desc_max_calls,
            non_interactive,
        } => {
            use forge_ingest::descriptions::config::{DescriptionsConfig, Provider};
            use forge_ingest::descriptions::orchestrator::{
                extract_descriptions, CandidateEntity, ExtractionInputs,
            };

            let cfg = DescriptionsConfig {
                enabled: true,
                provider: Provider::parse(&desc_provider)?,
                local_model: desc_model,
                local_endpoint: desc_endpoint,
                local_timeout_ms: desc_timeout_ms,
                remote_model: "claude-haiku-4-5".to_string(),
                max_words: 60,
                include_private: desc_include_private,
                min_confidence: desc_min_confidence,
                concurrency: desc_concurrency,
                max_desc_calls: desc_max_calls,
                non_interactive,
            };
            cfg.validate()?;

            eprintln!(
                "[descriptions] probing {} provider at {}…",
                desc_provider, cfg.local_endpoint
            );
            let ready = forge_ingest::descriptions::probe::check(&cfg)?;
            if ready.switched_from_config {
                eprintln!(
                    "[descriptions] using {} (switched from config at user prompt)",
                    ready.provider.label()
                );
            } else {
                eprintln!(
                    "[descriptions] probe ok: {} / {}",
                    ready.probe_info.provider_label, ready.probe_info.model
                );
            }

            let resolved_dir = forge_ingest::descriptions::project_root::resolve(&dir)?;
            if resolved_dir != dir {
                eprintln!(
                    "[descriptions] resolved '{}' → project root '{}'",
                    dir.display(),
                    resolved_dir.display()
                );
            }
            eprintln!("[descriptions] running Pass 1 (entity extraction)…");
            let report = forge_ingest::extractor::extract(&resolved_dir)?;
            eprintln!(
                "[descriptions] extracted {} entities (candidates for Pass 2)",
                report.entities.len()
            );

            // Adapt IngestReport entities to CandidateEntity. V1 treats
            // all extractor-produced entities as public; a richer shape
            // (pub/priv, doc_comment, body_head) is a followup.
            let candidates: Vec<CandidateEntity> = report
                .entities
                .iter()
                .map(|e| CandidateEntity {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    entity_type: e.entity_type.clone(),
                    is_public: true,
                    doc_comment: e.context.clone(),
                    body_head: String::new(),
                })
                .collect();

            eprintln!("[descriptions] running Pass 2 (description extraction)…");
            let out = extract_descriptions(
                &cfg,
                ready.provider,
                ExtractionInputs {
                    entities: candidates,
                },
            )?;
            eprintln!("[descriptions] {}", out.report.render());

            println!("{}", forge_shared::emit_json(&out, cli.pretty)?);
        }

        Commands::IngestUrl {
            url,
            depth,
            mcp_bin,
            session,
            tenant,
            dry_run,
        } => {
            let report = forge_ingest::url::extract_url_with_depth(&url, depth)?;
            if dry_run {
                println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
            } else {
                let load_report =
                    load_report_via_mcp(report, mcp_bin, session, tenant, "ingest-url")?;
                println!("{}", forge_shared::emit_json(&load_report, cli.pretty)?);
            }
        }
        Commands::IngestPaper {
            input,
            mcp_bin,
            session,
            tenant,
            dry_run,
        } => {
            let report = forge_ingest::paper::extract_paper(&input)?;
            if dry_run {
                println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
            } else {
                let load_report = load_paper_report_via_mcp(report, mcp_bin, session, tenant)?;
                println!("{}", forge_shared::emit_json(&load_report, cli.pretty)?);
            }
        }
        Commands::IngestCorpus {
            path,
            mcp_bin,
            session,
            tenant,
            dry_run,
        } => {
            let report = forge_ingest::corpus::extract_corpus(&path)?;
            if dry_run {
                println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
            } else {
                let load_report =
                    load_report_via_mcp(report, mcp_bin, session, tenant, "ingest-corpus")?;
                println!("{}", forge_shared::emit_json(&load_report, cli.pretty)?);
            }
        }

        Commands::Task { action } => {
            handle_task(action, cli.pretty)?;
        }

        Commands::FmemSkillIngest {
            root,
            filter,
            dry_run,
            session,
            force,
            server,
            verbose,
        } => {
            let exit = run_fmem_skill_ingest(
                root, filter, dry_run, session, force, server, verbose, cli.pretty,
            )?;
            std::process::exit(exit);
        }
        Commands::MermaidValidate => {
            let input = forge_shared::read_stdin().unwrap_or_default();
            let report = forge_mermaid_validate::validate(&input);
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::Checklist { action } => {
            let dir = std::env::current_dir()?;
            match action {
                ChecklistAction::Create { name, items } => {
                    let titles: Vec<String> =
                        items.split(',').map(|s| s.trim().to_string()).collect();
                    let cl = forge_checklist_state::create(&dir, &name, &titles)?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::CreateDag { name, file } => {
                    let text = std::fs::read_to_string(&file)?;
                    let cl: forge_checklist_state::Checklist = serde_json::from_str(&text)?;
                    let cl = forge_checklist_state::create_dag(&dir, &name, cl)?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::List => {
                    let names = forge_checklist_state::list(&dir)?;
                    println!("{}", forge_shared::emit_json(&names, cli.pretty)?);
                }
                ChecklistAction::Show { name } => {
                    let cl = forge_checklist_state::show(&dir, &name)?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::Validate { name } => {
                    let report = forge_checklist_state::validate(&dir, &name)?;
                    println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
                }
                ChecklistAction::Ready {
                    name,
                    limit,
                    include_expired_leases,
                } => {
                    let report =
                        forge_checklist_state::ready(&dir, &name, limit, include_expired_leases)?;
                    println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
                }
                ChecklistAction::Claim {
                    name,
                    agent,
                    limit,
                    lease_minutes,
                    include_expired_leases,
                } => {
                    let report = forge_checklist_state::claim(
                        &dir,
                        &name,
                        &agent,
                        limit,
                        lease_minutes,
                        include_expired_leases,
                    )?;
                    println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
                }
                ChecklistAction::Set {
                    name,
                    item_id,
                    status,
                } => {
                    let st = forge_checklist_state::ItemStatus::parse(&status)?;
                    let cl = forge_checklist_state::set(&dir, &name, &item_id, st)?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::Note {
                    name,
                    item_id,
                    text,
                } => {
                    let cl = forge_checklist_state::note(&dir, &name, &item_id, &text)?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::Release {
                    name,
                    item_id,
                    agent,
                } => {
                    let cl =
                        forge_checklist_state::release(&dir, &name, &item_id, agent.as_deref())?;
                    println!("{}", forge_shared::emit_json(&cl, cli.pretty)?);
                }
                ChecklistAction::Delete { name } => {
                    forge_checklist_state::delete(&dir, &name)?;
                    println!(
                        "{}",
                        forge_shared::emit_json(&serde_json::json!({"deleted": name}), cli.pretty)?
                    );
                }
            }
        }

        Commands::TodoExtract {
            path,
            no_blame,
            kinds,
        } => {
            let kinds_vec: Vec<String> = kinds
                .as_deref()
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default();
            let opts = forge_todo_extract::Options {
                blame: !no_blame,
                kinds: kinds_vec,
            };
            let report = forge_todo_extract::extract(&path, &opts)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::SecretScan { path } => {
            let opts = forge_secret_scan::Options {
                min_entropy: None,
                include_entropy: false,
            };
            let report = forge_secret_scan::scan(&path, &opts)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::DepsAudit { path, min_severity } => {
            let sev = forge_deps_audit::Severity::parse(&min_severity)?;
            let opts = forge_deps_audit::Options {
                offline: true,
                min_severity: sev,
            };
            let report = forge_deps_audit::audit(&path, &opts)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::ThreatScan {
            paths,
            categories,
            min_confidence,
        } => {
            let cats: Vec<forge_threat_scan::Category> = categories
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .filter_map(|t| forge_threat_scan::Category::parse(t.trim()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let conf = forge_threat_scan::Confidence::parse(&min_confidence)?;
            let opts = forge_threat_scan::Options {
                categories: cats,
                min_confidence: conf,
            };
            let report = forge_threat_scan::scan(&paths, &opts)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::FailLoudScan {
            paths,
            categories,
            min_confidence,
        } => {
            let cats: Vec<forge_fail_loud_scan::Category> = categories
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .filter_map(|t| forge_fail_loud_scan::Category::parse(t.trim()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let conf = forge_fail_loud_scan::Confidence::parse(&min_confidence)?;
            let opts = forge_fail_loud_scan::Options {
                categories: cats,
                min_confidence: conf,
            };
            let report = forge_fail_loud_scan::scan(&paths, &opts)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::SchemaDiff {
            before,
            after,
            dialect,
        } => {
            let before_src = std::fs::read_to_string(&before)?;
            let after_src = std::fs::read_to_string(&after)?;
            let dial = match dialect.as_deref() {
                Some("sql") => Some(forge_schema_diff::Dialect::Sql),
                Some("cql") => Some(forge_schema_diff::Dialect::Cql),
                Some("cypher") => Some(forge_schema_diff::Dialect::Cypher),
                Some(other) => anyhow::bail!("unknown dialect: {other}"),
                None => None,
            };
            let report = forge_schema_diff::diff_schemas(&before_src, &after_src, dial)?;
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::ApiDiff {
            before,
            after,
            lang,
        } => {
            let report = if before.is_file() && after.is_file() {
                let before_src = std::fs::read_to_string(&before)?;
                let after_src = std::fs::read_to_string(&after)?;
                let filename = after.file_name().and_then(|s| s.to_str()).unwrap_or("file");
                forge_api_diff::diff_sources(&before_src, &after_src, filename)?
            } else {
                forge_api_diff::diff_trees(&before, &after, lang.as_deref())?
            };
            println!("{}", forge_shared::emit_json(&report, cli.pretty)?);
        }

        Commands::ToolAliases { format } => {
            let aliases = crate::aliases::get_alias_map();
            if format == "table" {
                println!("{}", crate::aliases::format_as_table(&aliases));
            } else {
                println!("{}", forge_shared::emit_json(&aliases, cli.pretty)?);
            }
        }

        Commands::GlobStats {
            pattern,
            min_lines,
            max_lines,
            min_bytes,
            max_bytes,
            modified_after,
            modified_before,
            exclude,
            format,
            allow_absolute,
            max_results,
            max_depth,
            follow_links,
            no_gitignore,
        } => {
            // Flatten comma-separated `--exclude a,b` into individual entries.
            let user_excludes: Vec<String> = exclude
                .into_iter()
                .flat_map(|e| {
                    e.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
                .collect();
            crate::glob::run_from_cli(
                pattern,
                min_lines,
                max_lines,
                min_bytes,
                max_bytes,
                modified_after,
                modified_before,
                user_excludes,
                format,
                allow_absolute,
                max_results,
                max_depth,
                follow_links,
                no_gitignore,
                cli.pretty,
            )?;
        }

        Commands::Hook => {
            // Thin delegator: Claude Code Pre/PostToolUse hooks receive JSON on stdin.
            // PreToolUse: { tool_name, tool_input: { ... } }        — no tool_response
            // PostToolUse: { tool_name, tool_input: { ... }, tool_response: ... }
            let stdin_data = forge_shared::read_stdin().unwrap_or_default();
            if stdin_data.is_empty() {
                return Ok(());
            }

            // Parse the hook JSON from stdin.
            // Fail-loud: Claude Code sends structured JSON; if we received
            // something else, something is wrong (mis-configured hook,
            // upstream format change, truncated pipe). Log and exit cleanly
            // so the user's workflow isn't blocked, but leave a breadcrumb.
            let hook_json: serde_json::Value = match serde_json::from_str(&stdin_data) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "[forge hook] skipping: received non-JSON on stdin ({e}); \
                         first bytes: {}",
                        &stdin_data.chars().take(120).collect::<String>()
                    );
                    return Ok(());
                }
            };

            let tool_name = hook_json
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let is_post = hook_json.get("tool_response").is_some();

            match (tool_name, is_post) {
                // ── PreToolUse Read advisor (Feature 9) ──
                // Advise on large file reads before they happen.
                ("Read", false) => {
                    let file_path = hook_json
                        .get("tool_input")
                        .and_then(|v| v.get("file_path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if file_path.is_empty() {
                        return Ok(());
                    }

                    // Check if reading with no offset/limit (full file read)
                    let has_offset = hook_json
                        .get("tool_input")
                        .and_then(|v| v.get("offset"))
                        .is_some();
                    let has_limit = hook_json
                        .get("tool_input")
                        .and_then(|v| v.get("limit"))
                        .is_some();

                    if has_offset || has_limit {
                        // Targeted read — no advice needed
                        return Ok(());
                    }

                    // Check file size. Stat failures are surprising in a
                    // PreToolUse hook — the file is about to be read, so it
                    // should exist and be stat-able. Log before skipping.
                    let metadata = match std::fs::metadata(file_path) {
                        Ok(m) => m,
                        Err(e) => {
                            eprintln!(
                                "[forge hook] skipping advisor: cannot stat '{}': {e}",
                                file_path
                            );
                            return Ok(());
                        }
                    };
                    let file_bytes = metadata.len() as usize;
                    let est_tokens = file_bytes / 4;

                    // Only advise for files > 8K tokens (~32KB)
                    if est_tokens <= 8000 {
                        return Ok(());
                    }

                    // Check if this file was already read this session
                    let already_seen =
                        if let Ok(db_path) = forge_shared::tracking::default_db_path() {
                            if let Ok(conn) = forge_shared::tracking::open_db(&db_path) {
                                let session_id = std::env::var("CLAUDE_SESSION_ID")
                                    .unwrap_or_else(|_| format!("pid-{}", std::process::id()));
                                forge_shared::tracking::query_context(&conn, &session_id)
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|e| e.file_path == file_path)
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                    let mut hints = vec![format!(
                        "Note: {} is ~{}K tokens ({} bytes).",
                        file_path,
                        est_tokens / 1000,
                        file_bytes
                    )];

                    if already_seen {
                        hints.push(format!(
                            "This file was already read this session. Consider using `frg excerpt {}:<symbol>` for a targeted extract.",
                            file_path
                        ));
                    } else {
                        hints.push(format!(
                            "Consider `frg digest {}` for a structural overview first, or `frg excerpt {}:<symbol>` for a specific symbol.",
                            file_path, file_path
                        ));
                    }

                    println!("{}", hints.join(" "));
                }

                // ── PostToolUse Read tracker (Feature 7) ──
                // Record what was read; suggest alternatives for large reads.
                ("Read", true) => {
                    let file_path = hook_json
                        .get("tool_input")
                        .and_then(|v| v.get("file_path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if file_path.is_empty() {
                        return Ok(());
                    }

                    // Get response size
                    let response_text = hook_json
                        .get("tool_response")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                s.to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .unwrap_or_default();
                    let byte_count = response_text.len();

                    // Record in context tracking
                    if let Ok(db_path) = forge_shared::tracking::default_db_path() {
                        if let Ok(conn) = forge_shared::tracking::open_db(&db_path) {
                            let session_id = std::env::var("CLAUDE_SESSION_ID")
                                .unwrap_or_else(|_| format!("pid-{}", std::process::id()));
                            let offset = hook_json
                                .get("tool_input")
                                .and_then(|v| v.get("offset"))
                                .and_then(|v| v.as_u64())
                                .map(|v| v as usize);
                            let limit = hook_json
                                .get("tool_input")
                                .and_then(|v| v.get("limit"))
                                .and_then(|v| v.as_u64())
                                .map(|v| v as usize);
                            let _ = forge_shared::tracking::record_context(
                                &conn,
                                &session_id,
                                file_path,
                                None,                                   // symbol
                                offset,                                 // start_line
                                limit.map(|l| offset.unwrap_or(0) + l), // end_line
                                byte_count,
                            );
                        }
                    }
                }

                // ── PostToolUse Bash filter (existing) ──
                ("Bash", true) => {
                    let command = hook_json
                        .get("tool_input")
                        .and_then(|v| v.get("command"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if command.is_empty() {
                        return Ok(());
                    }

                    let tool_output = hook_json
                        .get("tool_response")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                s.to_string()
                            } else if let Some(stdout) = v.get("stdout").and_then(|s| s.as_str()) {
                                stdout.to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .unwrap_or_default();
                    if tool_output.is_empty() {
                        return Ok(());
                    }

                    let registry = forge_shared::filters::FilterRegistry::load();
                    let input_bytes = tool_output.len();
                    let project_dir = hook_json
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| {
                            std::env::current_dir()
                                .ok()
                                .map(|p| p.display().to_string())
                        });

                    // Detect and apply the appropriate filter (with fallback)
                    let fr = filter_output(&registry, command, &tool_output, cli.pretty);
                    let filter = fr.filter_name;
                    let output = fr.output;
                    let filter_ok = fr.success;
                    let filter_err = fr.error;
                    let duration_ms = fr.duration_ms;

                    // Print compressed output — Claude Code appends hook stdout as context
                    print!("{}", output);

                    // Track in filter_log
                    if let Ok(db_path) = forge_shared::tracking::default_db_path() {
                        if let Ok(conn) = forge_shared::tracking::open_db(&db_path) {
                            let _ = forge_shared::tracking::record_filter(
                                &conn,
                                &forge_shared::tracking::FilterRecord {
                                    command,
                                    filter_name: &filter,
                                    mode: forge_shared::tracking::InvocationMode::Hook,
                                    project_dir: project_dir.as_deref(),
                                    input_bytes,
                                    output_bytes: output.len(),
                                    duration_ms,
                                    filter_success: filter_ok,
                                    error_message: filter_err.as_deref(),
                                    exit_code: 0,
                                    tool_version: env!("CARGO_PKG_VERSION"),
                                },
                            );
                        }
                    }
                }

                // Unknown tool or phase — ignore
                _ => {}
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Task CLI handler
// ---------------------------------------------------------------------------

fn handle_task(action: TaskAction, pretty: bool) -> anyhow::Result<()> {
    match action {
        TaskAction::Create {
            title,
            body,
            assignee,
            priority,
            workspace,
            workspace_path,
            metadata,
            created_by,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let req = forge_tasks::CreateTaskRequest {
                title,
                body,
                assignee,
                reviewer: None,
                priority,
                workspace_kind: workspace,
                workspace_path,
                metadata,
                created_by,
                skills: None,
                parents: None,
            };
            let task = store.create_task(req)?;
            println!("{}", forge_shared::emit_json(&task, pretty)?);
        }

        TaskAction::Update {
            task_id,
            status,
            assignee,
            priority,
            title,
            body,
            block_reason,
            result,
            summary,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let patch = forge_tasks::UpdateTaskPatch {
                status,
                assignee,
                reviewer: None,
                priority,
                title,
                body,
                block_reason,
                result,
                summary,
            };
            let task = store.update_task(&task_id, patch)?;
            println!("{}", forge_shared::emit_json(&task, pretty)?);
        }

        TaskAction::Get { task_id, cql_host } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let task = store.get_task(&task_id)?;
            println!("{}", forge_shared::emit_json(&task, pretty)?);
        }

        TaskAction::List {
            status,
            assignee,
            priority_gte,
            priority_lte,
            limit,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let filter = forge_tasks::TaskFilter {
                status,
                assignee,
                priority_gte,
                priority_lte,
                limit: Some(limit),
            };
            let tasks = store.list_tasks(filter)?;
            println!("{}", forge_shared::emit_json(&tasks, pretty)?);
        }

        TaskAction::Link {
            parent_id,
            child_id,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            store.link_tasks(&parent_id, &child_id, "child")?;
            println!("Linked {} \u{2192} {}", parent_id, child_id);
        }

        TaskAction::Unlink {
            parent_id,
            child_id,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            store.unlink_tasks(&parent_id, &child_id)?;
            println!("Unlinked {} \u{2194} {}", parent_id, child_id);
        }

        TaskAction::Comment {
            task_id,
            body,
            author,
            cql_host,
        } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let comment = store.add_comment(&task_id, &author, &body)?;
            println!("{}", forge_shared::emit_json(&comment, pretty)?);
        }

        TaskAction::Board { cql_host } => {
            let store = forge_tasks::TaskStore::connect(
                &forge_tasks::resolve_cql_host(cql_host.as_deref()),
                None,
            )?;
            let board = store.board()?;
            println!("{}", forge_shared::emit_json(&board, pretty)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod ferrosa_memory_config_tests {
    use super::FerrosaMemoryConfig;

    fn config(require_tls: bool) -> FerrosaMemoryConfig {
        FerrosaMemoryConfig {
            tenant_id: None,
            session_id: None,
            transport: Some("http".into()),
            require_tls,
            http_bind_addr: Some("0.0.0.0".into()),
            http_port: Some(18765),
            http_username: None,
            http_password: None,
            http_timeout_ms: None,
            max_rows_per_chunk: None,
        }
    }

    #[test]
    fn http_base_url_uses_https_when_require_tls_is_true() {
        assert_eq!(
            config(true).http_base_url().as_deref(),
            Some("https://127.0.0.1:18765")
        );
    }

    #[test]
    fn http_base_url_uses_http_when_require_tls_is_false() {
        assert_eq!(
            config(false).http_base_url().as_deref(),
            Some("http://127.0.0.1:18765")
        );
    }
}
