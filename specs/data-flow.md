# Forge Data Flow

> Last updated: 2026-07-21
> Status: Current runtime reference

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
    participant CLI as frg --mcp / --mcp-http
    participant Detect as project-detect
    participant Server as mcp-server

    Client->>CLI: server/discover or initialize
    CLI->>Detect: detect current project stack
    CLI->>Server: set detected stacks
    Client->>Server: tools/list
    Server-->>Client: visible tools with input/output schemas and result metadata
```

### Streamable HTTP MCP Flow

```mermaid
sequenceDiagram
    participant Client
    participant HTTP as POST /mcp
    participant Server as mcp-server
    Client->>HTTP: JSON-RPC request + MCP metadata and mirrored headers
    HTTP->>HTTP: enforce body cap, method, Origin, and header/body agreement
    HTTP->>Server: validated request
    Server-->>HTTP: structured MCP result or JSON-RPC error
    HTTP-->>Client: JSON response
```

`GET` and `DELETE` at `/mcp` deliberately return `405 Method Not Allowed`.
The HTTP endpoint is for modern MCP clients that can supply the required draft
metadata; stdio remains the simplest client configuration.

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
        Client->>Fmem: JSON-RPC tools/call over stdio or HTTP
    end

    Note over CLI,Fmem: Phase B — skill ingest (fmem auto-creates tags)
    loop per skill
        CLI->>Client: ingest_skill(args + content_hash)
        Client->>Fmem: JSON-RPC tools/call over stdio or HTTP
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
this flow; its external skill-ingestion operations remain gated on matching
ferrosa-memory support.

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
- stdio and HTTP MCP dispatch share the same server and tool handlers
- `crates/cli` remains the only orchestration entrypoint
- ingestion still supports code, web, and paper modes
- hook flow still delegates through the canonical hook command
