# Riak Architecture Analysis

Design Structure Matrix (DSM) analysis of Riak's core components using `frg dsm`.
Riak is a distributed key-value database built on Erlang/OTP, implementing
Amazon Dynamo-style consistent hashing, vnodes, and eventually-consistent replication.

## Analysis Scope

| Component | Path | Modules | Dependencies | Language |
|-----------|------|---------|--------------|----------|
| riak_core | `/tmp/riak_core` | 124 | 443 | Erlang |
| riak_kv | `/tmp/riak_kv` | 167 | 574 | Erlang |

---

## riak_core

### Summary Metrics

| Metric | Value | Status |
|--------|-------|--------|
| Elements | 124 | - |
| Dependencies | 443 | - |
| Propagation Cost | 34.6% | critical |
| Max Cycle Size | 45 | critical |
| Number of Cycles | 1 | - |
| Cluster Quality | 54.4% | warning |

**Propagation cost of 34.6%** means that on average, a change to one module
may propagate to ~43 of the 124 modules. The single 45-module cycle is the
primary contributor — over a third of all modules are mutually dependent.

### Dependency Graph (Collapsed)

Auto-generated collapsed view — each node represents a cluster of related modules.
Edge labels show the number of inter-cluster dependencies; dashed edges indicate
cycle participation. 124 modules across 24 clusters.

```mermaid
graph LR
    c0["riak_core_bg_manager (3)"]
    c1["riak_core_repair"]
    c2["gen_nb_server (3)"]
    c3["riak_core_priority_queue"]
    c4["riak_core_eventhandler_sup"]
    c5["riak_core_app (3)"]
    c6["riak_core_broadcast_handler"]
    c7["riak_core_claim_binring_alg (2)"]
    c8["riak_core_claim_util"]
    c9["riak_core_tcp_mon"]
    c10["app_helper (26)"]
    c11["riak_core_dtrace"]
    c12["exometer_entry (3)"]
    c13["clique_handler (2)"]
    c14["riak_core_handoff (2)"]
    c15["vclock (23)"]
    c16["riak_core_metadata_manager (5)"]
    c17["chash (28)"]
    c18["riak_core_net_ticktime"]
    c19["riak_core_pw_auth (3)"]
    c20["riak_core_sysmon* (2)"]
    c21["riak_core_throttle (3)"]
    c22["hashtree (5)"]
    c23["pulse (3)"]
    c15 -.->|"CYCLE (32)"| c17
    c17 -.->|"CYCLE (24)"| c15
    c10 -.->|"CYCLE (22)"| c17
    c15 -.->|"CYCLE (14)"| c10
    c17 -.->|"CYCLE (13)"| c10
    c16 -.->|"CYCLE (10)"| c17
    c10 -.->|"CYCLE (6)"| c15
    c17 -.->|"CYCLE (5)"| c16
    c10 -.->|"CYCLE (4)"| c23
    c22 -.->|"CYCLE (3)"| c17
    c15 -.->|"CYCLE (2)"| c8
    c23 -.->|"CYCLE (2)"| c10
    c8 -.->|"CYCLE (2)"| c15
    c16 -.->|"CYCLE (2)"| c22
    c8 -.->|CYCLE| c17
    c5 -->|"6"| c17
    c2 -->|"6"| c10
    c10 -->|"5"| c14
    c17 -->|"3"| c4
    c15 -->|"3"| c22
    c5 -->|"2"| c10
    c19 -->|"2"| c10
    c19 -->|"2"| c17
    c16 -->|"2"| c10
    c5 -->|"2"| c15
    c16 -->|"2"| c15
    c17 -->|"2"| c22
    c1 --> c10
    c0 --> c10
    c15 --> c7
    c18 --> c17
    c12 --> c10
    c5 --> c0
    c9 --> c17
    c22 --> c15
    c10 --> c3
    c11 --> c17
    c15 --> c19
    c16 --> c6
    c0 --> c22
    c21 --> c10
    c15 --> c14
    c16 --> c19
    c5 --> c21
    c22 --> c10
    c15 --> c13
    c13 --> c10
    c1 --> c17
    c15 --> c5
    c1 --> c15
```

Key clusters: **c17** = chash/ring/capability/gossip/metadata (28 modules),
**c15** = vclock/claimant/membership/ring (23 modules),
**c10** = app_helper/vnode/handoff/stat (26 modules).
The heaviest cycle edges (32, 24, 22 deps) flow between these three clusters.

### Cycle Analysis

**1 cycle involving 45 modules (36% of codebase):**

The mega-cycle spans ring management, vnode layer, cluster membership, handoff,
metadata, and stats — effectively the entire core. Key cycle drivers:

| Module | Fan-in | Fan-out | Role in Cycle |
|--------|--------|---------|---------------|
| `riak_core_ring` | high | high | Central data structure, referenced by nearly everything |
| `riak_core_vnode` | high | high | Generic vnode behavior, couples to ring + handoff |
| `riak_core_util` | high | medium | Utility grab-bag, pulls in ring for convenience |
| `riak_core_gossip` | medium | high | Cluster coordination, touches ring + membership |
| `riak_core_claimant` | medium | high | Partition assignment, couples ring + membership |

**Root cause**: `riak_core_ring` is both a data structure and a coordination point.
Modules that should only read ring state also depend on modules that write to it,
creating circular paths through the ring manager, gossip, and claimant.

---

## riak_kv

### Summary Metrics

| Metric | Value | Status |
|--------|-------|--------|
| Elements | 167 | - |
| Dependencies | 574 | - |
| Propagation Cost | 32.8% | critical |
| Max Cycle Size | 49 | critical |
| Number of Cycles | 2 | - |
| Cluster Quality | 33.6% | critical |

### Dependency Graph (Collapsed)

Auto-generated collapsed view — 167 modules across 46 clusters.
Edge labels show inter-cluster dependency counts; dashed edges indicate cycles.

```mermaid
graph LR
    c0["riak_object (10)"]
    c1["riak_kv_yessir_backend"]
    c2["bitcask"]
    c3["eqc_component"]
    c4["eqc_fsm"]
    c5["riak_object_dvv_statem (3)"]
    c6["exometer_entry"]
    c7["json_pp (5)"]
    c8["riak_dt_pb (18)"]
    c9["riak_kv_ensembles (2)"]
    c10["riak_kv_worker (2)"]
    c11["riak_kv_bitcask_backend"]
    c12["riak_kv_crdt (7)"]
    c13["riak_core_pb (7)"]
    c14["riak_kv_delete (2)"]
    c15["riak_kv_dtrace (5)"]
    c16["riak_ensemble_types (2)"]
    c17["riak_kv_eraser (2)"]
    c18["riak_kv_app (3)"]
    c19["riak_kv_exometer_sidejob"]
    c20["riak_kv_get_fsm (2)"]
    c21["riak_kv_gcounter (3)"]
    c22["riak_kv_cinfo (5)"]
    c23["riak_dt"]
    c24["riak_kv_hotbackup_fsm"]
    c25["riak_kv_tictacaae_repairs"]
    c26["riak_kv_mrc_pipe (5)"]
    c27["riak_kv_pb* (2)"]
    c28["riak (8)"]
    c29["stacktrace (4)"]
    c30["riak_kv_put_fsm (2)"]
    c31["riak_kv_reaper (3)"]
    c32["riak_kv_replrtq_src (7)"]
    c33["riak_kv_requests (5)"]
    c34["riak_kv_stat (6)"]
    c35["riak_kv_stat_worker"]
    c36["riak_kv_hll (4)"]
    c37["riak_kv_w1c* (2)"]
    c38["webmachine (11)"]
    c39["riak_kv_2i_aae (2)"]
    c40["riak_ensemble_backend (2)"]
    c41["riak_kv_mrc_sink (4)"]
    c42["riak_pipe (7)"]
    c43["sms (2)"]
    c44["tracer_read_bin_trace_file"]
    c45["raw_link_walker"]
    c28 -.->|"CYCLE (11)"| c8
    c26 -.->|"CYCLE (9)"| c42
    c8 -.->|"CYCLE (8)"| c34
    c8 -.->|"CYCLE (7)"| c0
    c28 -.->|"CYCLE (6)"| c0
    c8 -.->|"CYCLE (6)"| c28
    c12 -.->|"CYCLE (5)"| c0
    c8 -.->|"CYCLE (5)"| c32
    c32 -.->|"CYCLE (5)"| c0
    c29 -.->|"CYCLE (4)"| c0
    c33 -.->|"CYCLE (4)"| c0
    c13 -.->|"CYCLE (4)"| c28
    c42 -.->|"CYCLE (4)"| c26
    c13 -.->|"CYCLE (4)"| c0
    c36 -.->|"CYCLE (3)"| c12
    c15 -.->|"CYCLE (3)"| c13
    c33 -.->|"CYCLE (3)"| c13
    c0 -.->|"CYCLE (3)"| c13
    c15 -.->|"CYCLE (3)"| c33
    c13 -.->|"CYCLE (3)"| c32
    c30 -.->|"CYCLE (3)"| c0
    c22 -.->|"CYCLE (3)"| c0
    c28 -.->|"CYCLE (3)"| c22
    c13 -.->|"CYCLE (3)"| c22
    c8 -.->|"CYCLE (2)"| c15
    c20 -.->|"CYCLE (2)"| c34
    c25 -.->|"CYCLE (2)"| c28
    c25 -.->|"CYCLE (2)"| c0
    c13 -.->|"CYCLE (2)"| c33
    c20 -.->|"CYCLE (2)"| c0
    c14 -.->|"CYCLE (2)"| c0
    c38 -.->|"CYCLE (2)"| c28
    c22 -.->|"CYCLE (2)"| c28
    c7 -.->|"CYCLE (2)"| c38
    c8 -.->|"CYCLE (2)"| c31
    c13 -.->|"CYCLE (2)"| c36
    c20 -.->|"CYCLE (2)"| c32
    c30 -.->|"CYCLE (2)"| c13
    c22 -.->|"CYCLE (2)"| c13
    c34 -.->|"CYCLE (2)"| c7
    c34 -.->|"CYCLE (2)"| c0
    c30 -.->|"CYCLE (2)"| c34
    c0 -.->|"CYCLE (2)"| c12
    c30 -.->|"CYCLE (2)"| c29
    c12 -.->|"CYCLE (2)"| c36
    c8 -.->|"CYCLE (2)"| c13
    c37 -.->|"CYCLE (2)"| c0
    c8 -.->|CYCLE| c20
    c13 -.->|CYCLE| c40
    c24 -.->|CYCLE| c33
    c37 -.->|CYCLE| c34
    c34 -.->|CYCLE| c33
    c37 -.->|CYCLE| c13
    c17 -.->|CYCLE| c14
    c34 -.->|CYCLE| c12
    c8 -.->|CYCLE| c33
    c31 -.->|CYCLE| c0
    c29 -.->|CYCLE| c22
    c25 -.->|CYCLE| c8
    c20 -.->|CYCLE| c13
```

Key clusters: **c0** = riak_object/backends/util (10 modules),
**c8** = riak_dt_pb/client/replrtq_snk/ttaaefs (18 modules),
**c13** = riak_core_pb/vnode/kv_vnode (7 modules),
**c28** = riak/index/exchange_fsm/reader (8 modules).
The heaviest cycle edges (11, 9, 8 deps) flow between c28↔c8, c26↔c42 (MapReduce), and c8↔c34 (stats).

### Cycle Analysis

**Cycle 1: 49 modules (29% of codebase)**

The primary cycle spans the vnode, FSMs, AAE, replication, and stats layers.
Key coupling points:

- `riak_kv_vnode` depends on `riak_kv_stat` (for instrumentation)
  which depends on `riak_kv_status` which depends on `riak_kv_vnode`
- `riak_kv_util` is a utility module referenced by almost everything,
  but it also reads from the vnode layer
- `riak_object` is coupled to `riak_kv_crdt` which is coupled to `riak_kv_vnode`

**Cycle 2: 6 modules (MapReduce pipeline)**

```
riak_kv_mrc_map <-> riak_kv_mrc_pipe <-> riak_kv_pipe_get
riak_kv_mrc_pipe <-> riak_kv_w_reduce
riak_kv_mrc_pipe <-> riak_kv_pipe_index
riak_kv_mrc_pipe <-> riak_kv_pipe_listkeys
```

This is a tighter, more contained cycle within the MapReduce subsystem.

---

## Dead Code Analysis

### riak_core

| Metric | Value |
|--------|-------|
| Total declarations | 3,955 |
| Entry points | 226 |
| Reachable symbols | 3,668 |
| Definitely dead | 67 (1.7%) |
| Possibly dead | 184 (4.7%) |

**Definite dead code by module:**

| Module | Dead Functions | Description |
|--------|---------------|-------------|
| `riak_core_handoff_status` | 25 | Entire module appears dead — no exported functions are called. Likely replaced by CLI-based handoff reporting. |
| `bg_manager_tests` | 16 | Test helper functions not invoked by EUnit generators. May use EUnit macros not detected by static analysis. |
| `riak_core_repair` | 2 | `make_nowrap/4`, `make_wrap/4` — private repair functions not called anywhere. |
| `riak_core_stat_xform` | 1 | `transform/1` — exometer stat transform callback, possibly registered dynamically. |
| `riak_core_security_tests` | 1 | `start_manager/0` — test setup function. |

**Note:** The 184 "possibly dead" items are public functions not referenced
internally. Many are likely used by dependent applications (riak_kv, riak_pipe, etc.)
via remote calls `riak_core_*:function()`.

### riak_kv

| Metric | Value |
|--------|-------|
| Total declarations | 4,785 |
| Entry points | 136 |
| Reachable symbols | 4,514 |
| Definitely dead | 0 |
| Possibly dead | 195 (4.1%) |

No definitely dead code in riak_kv — all private functions are reachable.
The 195 possibly dead items are public functions not referenced internally,
likely used by HTTP/PB API handlers, riak_pipe, or other OTP applications.

---

## Combined Architecture Observations

### 1. God Module Anti-Pattern

Both `riak_core_ring` and `riak_kv_vnode` exhibit god-module characteristics:
- High fan-in (many modules depend on them)
- High fan-out (they depend on many modules)
- They are the primary drivers of the mega-cycles

### 2. Utility Module Coupling

`riak_core_util` and `riak_kv_util` create unnecessary coupling by mixing
unrelated utilities that pull in heavy dependencies. Splitting these into
focused modules would reduce propagation cost.

### 3. Stats/Instrumentation Cycles

In both components, stats modules create cycles by importing the modules
they instrument. Using a callback/event pattern instead would break these cycles.

### 4. Cluster Quality

| Component | Cluster Quality | Assessment |
|-----------|----------------|------------|
| riak_core | 54.4% | Warning — moderate modularity |
| riak_kv | 33.6% | Critical — poor module boundaries |

riak_kv's lower cluster quality reflects its flatter architecture where
most modules can reach most other modules within 2-3 hops.

### 5. Comparison to Other Distributed Databases

| Metric | riak_core | riak_kv | Typical Threshold |
|--------|-----------|---------|-------------------|
| Propagation Cost | 34.6% | 32.8% | < 20% good, < 30% acceptable |
| Max Cycle Size | 45 (36%) | 49 (29%) | < 10% of modules |
| Cluster Quality | 54.4% | 33.6% | > 70% good |

Both components exceed typical thresholds. The large cycles are characteristic
of organically-grown distributed systems where cross-cutting concerns
(ring, gossip, stats) were added incrementally rather than designed as
isolated layers.

---

*Generated by `frg dsm` — Erlang/OTP dependency extraction with
Tarjan SCC detection, Thebeau clustering, and BFS dead-code analysis.*
