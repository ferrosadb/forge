# Forge Data Flow

> Last updated: 2026-04-07
> Status: Draft

## Primary Runtime Flows

### CLI and MCP Request Flow

```mermaid
sequenceDiagram
    participant User
    participant Binary as frg CLI
    participant Library as feature crate
    participant Shared as shared/tracking
    participant Output as stdout/JSON

    User->>Binary: subcommand or MCP tool call
    Binary->>Library: invoke feature handler
    Library-->>Binary: structured result or error
    Binary->>Shared: optional analytics/tracking update
    Binary-->>Output: JSON or pretty output
```

### MCP Tool Discovery Flow

```mermaid
sequenceDiagram
    participant Client
    participant CLI as frg --mcp
    participant Detect as project-detect
    participant Server as mcp-server

    Client->>CLI: initialize
    CLI->>Detect: detect current project stack
    CLI->>Server: set detected stacks
    Client->>Server: tools/list
    Server-->>Client: tier-1 + matching tier-2 tools
```

### Proxy and Hook Flow

```mermaid
graph TD
    A[Claude hook or user runs frg run] --> B[detect command/filter]
    B --> C[execute underlying command]
    C --> D[parse raw output with filter crate]
    D --> E[record analytics]
    E --> F[emit compact JSON or pretty summary]
```

### Ingestion Flow

```mermaid
graph TD
    A[input: path or URL or paper] --> B[ingest crate extracts entities + edges]
    B --> C{ferrosa-memory config / cql provided?}
    C -- yes --> D[loader writes graph]
    C -- no --> E[return IngestReport JSON]
    D --> F[result summary]
    E --> F
```

### Skill Catalog Ingestion Flow (`frg fmem-skill-ingest`)

```mermaid
sequenceDiagram
    participant User
    participant CLI as frg fmem-skill-ingest
    participant Walk as skill_ingest::walk
    participant Parse as skill_ingest::parse
    participant Tax as skill_ingest::taxonomy
    participant Client as fmem-client
    participant Fmem as ferrosa-memory

    User->>CLI: frg fmem-skill-ingest [flags]
    CLI->>Walk: walk(skill_root)
    Walk-->>CLI: Vec<SkillFile>
    CLI->>Parse: parse each file
    Parse-->>CLI: Vec<Skill>
    CLI->>Tax: build_plan(root, skills)
    Tax-->>CLI: TaxonomyPlan { tags, edges }

    Note over CLI,Fmem: Phase A — taxonomy seed
    loop per PARENT_TAG edge in plan
        CLI->>Client: ensure_parent_tag(child, parent)
        Client->>Fmem: JSON-RPC tools/call
    end

    Note over CLI,Fmem: Phase B — skill ingest (fmem auto-creates tags)
    loop per skill
        CLI->>Client: ingest_skill(args + content_hash)
        Client->>Fmem: JSON-RPC tools/call
        Fmem-->>Client: Created / Updated / Skipped
    end

    Note over CLI,Fmem: Phase C — re-pass for skipped REQUIRES
    CLI->>Client: re-ingest_skill for skills whose prereqs now exist

    Note over CLI,Fmem: Phase D — verify (exit gate)
    loop per skill
        CLI->>Client: verify_skill(name)
        Client-->>CLI: tags, prerequisites, missing_prerequisites
    end

    CLI-->>User: summary + exit code
```

See `specs/fmem-skill-ingest/` for the full blueprint of
this flow; `ensure_parent_tag` and `verify_skill` wrappers land once
`../../../ferrosa-memory/specs/todo/skill-ingest-support.md` ships.

## Important Data Paths

### Analytics

- Raw command sizes and filtered output sizes flow through `shared::tracking`
- Storage target is a local SQLite database
- Reporting surfaces through `gain`, `analytics`, and `clear-analytics`

### Configuration

- CLI and loaders read local config files for hook state, filters, and ferrosa-memory connectivity
- Hook generation writes canonical settings that delegate behavior back to the binary

### Knowledge Graph Output

- `ingest`, `ingest-url`, and `ingest-paper` all normalize into `IngestReport`
- Loader path branches on configured CQL contact points
- Sanitization strips unsafe or sensitive fields before persistence

## Drift Checks

Architecture updates should verify these still match the code:

- MCP tool tiering is enforced in `crates/mcp-server`
- `crates/cli` remains the only orchestration entrypoint
- ingestion still supports code, web, and paper modes
- hook flow still delegates through the canonical hook command
