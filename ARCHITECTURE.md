# Rewind Architecture Deep Dive

> **Purpose**: Comprehensive technical reference for a senior staff-level architecture discussion. Covers high-level system design, low-level component internals, data models, algorithms, and design trade-offs.

---

## Table of Contents

1. [What Rewind Is](#1-what-rewind-is)
2. [High-Level Architecture](#2-high-level-architecture)
3. [Workspace & Crate Map](#3-workspace--crate-map)
4. [Data Model (The Schema)](#4-data-model-the-schema)
5. [Storage Layer: rewind-store](#5-storage-layer-rewind-store)
6. [Recording Layer: rewind-proxy](#6-recording-layer-rewind-proxy)
7. [Analysis Layer: rewind-replay](#7-analysis-layer-rewind-replay)
8. [Assertion Layer: rewind-assert](#8-assertion-layer-rewind-assert)
9. [MCP Server: rewind-mcp](#9-mcp-server-rewind-mcp)
10. [CLI & TUI: rewind-cli / rewind-tui](#10-cli--tui-rewind-cli--rewind-tui)
11. [Python SDK: rewind-agent](#11-python-sdk-rewind-agent)
12. [Key Algorithms](#12-key-algorithms)
13. [Concurrency & Thread Safety](#13-concurrency--thread-safety)
14. [Streaming Architecture](#14-streaming-architecture)
15. [Cost Tracking](#15-cost-tracking)
16. [CI/CD & Distribution](#16-cicd--distribution)
17. [Design Decisions & Trade-offs](#17-design-decisions--trade-offs)
18. [Roadmap & Known Gaps](#18-roadmap--known-gaps)

---

## 1. What Rewind Is

Rewind is a **time-travel debugger for AI agents**. Think "Chrome DevTools for LLM agent traces."

**Problem**: When an AI agent makes 15 LLM calls, uses 8 tools, and produces a wrong answer — you have no way to see *where* it went wrong, *what* the model saw at each step, or *what would have happened* if you changed one tool response.

**Solution**: Rewind intercepts every LLM API call, records the full request/response with cost and token metadata, and provides tools to inspect, fork, diff, and replay those execution traces.

```
                    ┌───────────────────────────────────────────────────┐
                    │                   REWIND                          │
                    │                                                   │
                    │   Record ──► Inspect ──► Fork ──► Diff ──► Replay │
                    │                                                   │
                    │   "What did the model see at step 7?"             │
                    │   "What if the tool returned different data?"     │
                    │   "Where did costs spike?"                        │
                    └───────────────────────────────────────────────────┘
```

**Key properties**:

- Single binary, 9 MB, zero runtime dependencies
- Works with OpenAI, Anthropic, AWS Bedrock, any OpenAI-compatible API
- Two recording modes: HTTP proxy (language-agnostic) or Python SDK (in-process)
- Shared SQLite + content-addressed blob store (both modes write identical format)
- MIT licensed, public repo

---

## 2. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        USER'S AI AGENT                                  │
│  (Python, TypeScript, Rust, any language)                               │
└──────────┬─────────────────────────────┬────────────────────────────────┘
           │                             │
     ┌─────▼──────┐              ┌───────▼────────┐
     │ Direct Mode │              │  Proxy Mode    │
     │ (Python SDK)│              │ (HTTP Proxy)   │
     │             │              │                │
     │ Monkey-patch│              │ MITM intercept │
     │ OpenAI/     │              │ on port 8443   │
     │ Anthropic   │              │                │
     │ SDK methods │              │ Forward to     │
     └─────┬───────┘              │ upstream API   │
           │                      └───────┬────────┘
           │                              │
           └──────────┬───────────────────┘
                      │
                      ▼
          ┌───────────────────────┐
          │    ~/.rewind/         │
          │                       │
          │  rewind.db (SQLite)   │   ◄── Sessions, Timelines, Steps
          │  objects/  (Blobs)    │   ◄── Content-addressed request/response
          └───────────┬───────────┘
                      │
        ┌─────────────┼─────────────────────────┐
        │             │                         │
  ┌─────▼─────┐ ┌────▼──────┐  ┌───────────────▼───────────┐
  │  CLI      │ │  TUI      │  │  MCP Server               │
  │           │ │           │  │  (Claude/Cursor/Windsurf)  │
  │ sessions  │ │ Two-panel │  │                            │
  │ show      │ │ inspector │  │  list_sessions             │
  │ inspect   │ │ with kbd  │  │  show_session              │
  │ fork      │ │ nav       │  │  get_step_detail           │
  │ diff      │ │           │  │  diff_timelines            │
  │ assert    │ │           │  │  fork_timeline             │
  │ snapshot  │ │           │  │  create/check/list/show/   │
  │ cache     │ │           │  │  delete_baseline           │
  │ demo      │ │           │  │  list_snapshots            │
  └───────────┘ └───────────┘  │  cache_stats               │
                               └────────────────────────────┘
```

### Two Recording Modes


|               | **Direct Mode** (Python SDK)                      | **Proxy Mode** (HTTP Proxy)                      |
| ------------- | ------------------------------------------------- | ------------------------------------------------ |
| **How**       | Monkey-patches `openai` / `anthropic` SDK methods | HTTP MITM proxy on configurable port             |
| **Language**  | Python only                                       | Any language                                     |
| **Setup**     | `rewind_agent.init()` — one line                  | `rewind record --port 8443` + set `BASE_URL` env |
| **Latency**   | Zero (in-process write to SQLite)                 | ~1-5ms (extra HTTP hop)                          |
| **Features**  | Records LLM calls, cost tracking                  | Records + Instant Replay cache                   |
| **Streaming** | Full support (accumulates chunks)                 | Full support (forwards SSE in real-time)         |


Both modes write to the **exact same storage format** (`~/.rewind/rewind.db` + `~/.rewind/objects/`), so all inspection tools (CLI, TUI, MCP) work identically regardless of recording mode.

---

## 3. Workspace & Crate Map

```
rewind/
├── Cargo.toml                    # Workspace root (Rust 2024 edition, resolver v2)
├── crates/
│   ├── rewind-store/             # Data persistence layer
│   │   ├── src/models.rs         #   Data model structs (Session, Timeline, Step, etc.)
│   │   ├── src/db.rs             #   SQLite operations + migrations
│   │   ├── src/blobs.rs          #   Content-addressed blob storage
│   │   └── src/lib.rs            #   Public API re-exports
│   │
│   ├── rewind-proxy/             # HTTP proxy for recording
│   │   ├── src/lib.rs            #   ProxyServer, request handling, SSE parsing
│   │   └── src/main.rs           #   (unused — proxy started from CLI)
│   │
│   ├── rewind-replay/            # Analysis engine
│   │   └── src/lib.rs            #   ReplayEngine: fork, diff, full timeline reconstruction
│   │
│   ├── rewind-assert/            # Regression testing engine
│   │   ├── src/lib.rs            #   Public API re-exports
│   │   ├── src/tolerance.rs      #   Configurable check tolerances
│   │   ├── src/extract.rs        #   Tool name + response fingerprint extraction
│   │   ├── src/baseline.rs       #   BaselineManager: create, list, get, delete baselines
│   │   └── src/checker.rs        #   AssertionEngine: 8 checks per step, 5 verdict types
│   │
│   ├── rewind-mcp/               # MCP protocol server
│   │   ├── src/main.rs           #   Entry point (stdio transport)
│   │   └── src/server.rs         #   Tool handlers
│   │
│   ├── rewind-cli/               # CLI binary
│   │   └── src/main.rs           #   clap commands, session resolution, formatting
│   │
│   └── rewind-tui/               # Terminal UI
│       └── src/lib.rs            #   ratatui two-panel inspector
│
├── python/                       # Python SDK
│   ├── pyproject.toml            #   Package config (zero deps, hatchling)
│   ├── rewind_agent/
│   │   ├── __init__.py           #   Public API (init, step, tool, node, trace, annotate)
│   │   ├── patch.py              #   Mode management (direct vs proxy)
│   │   ├── recorder.py           #   Monkey-patching recorder (558 lines, core logic)
│   │   ├── store.py              #   Pure Python SQLite + blob store (format-compatible)
│   │   ├── hooks.py              #   Decorators (@step, @tool, @node) + framework adapters
│   │   ├── assertions.py         #   Assertions class — baseline querying + regression checks
│   │   └── _cli.py               #   CLI bootstrap (downloads/caches native binary)
│   └── tests/
│       ├── test_store.py         #   Store unit tests (thread safety, blob dedup)
│       ├── test_recorder.py      #   Recorder tests (streaming, concurrency)
│       └── test_assertions.py    #   Assertion tests (baselines, check verdicts)
│
├── README.md                     #   Comprehensive user-facing docs
├── PLAN-direct-recording.md      #   Feature plan for direct recording mode
├── .github/workflows/
│   ├── ci.yml                    #   CI: build + clippy + integration test + Python import check
│   └── release.yml               #   Release: 4-platform binary build + GitHub release
└── .mcp.json                     #   MCP server config for local dev
```

### Dependency Graph (Crates)

```
rewind-cli ──────► rewind-proxy ──► rewind-store
    │                                    ▲
    ├──────────► rewind-replay ──────────┘
    ├──────────► rewind-assert ──► rewind-store
    │                              rewind-replay
    ├──────────► rewind-tui ─────► rewind-store
    │                              rewind-replay
    └──────────► rewind-store

rewind-mcp ─────► rewind-store
    ├───────────► rewind-replay
    └───────────► rewind-assert
```

**Key insight**: `rewind-store` is the foundation — every other crate depends on it. `rewind-proxy` and `rewind-replay` are independent peers. `rewind-cli` is the integration point that pulls everything together.

---

## 4. Data Model (The Schema)

### Entity Relationship

```
┌─────────────┐       ┌──────────────┐       ┌─────────────┐
│   Session    │ 1───* │   Timeline   │ 1───* │    Step     │
│              │       │              │       │             │
│ id (UUID)    │       │ id (UUID)    │       │ id (UUID)   │
│ name         │       │ session_id   │       │ timeline_id │
│ status       │       │ parent_id ───┼──self │ session_id  │
│ total_steps  │       │ fork_at_step │       │ step_number │
│ total_cost   │       │ label        │       │ step_type   │
│ total_tokens │       │              │       │ status      │
│ metadata {}  │       └──────────────┘       │ model       │
└─────────────┘                               │ tokens_in   │
                                              │ tokens_out  │
                                              │ cost_usd    │
                                              │ duration_ms │
                                              │ request_blob│──► Blob Store
                                              │ response_blob──► Blob Store
                                              │ error       │
                                              └─────────────┘

┌────────────────┐       ┌──────────────┐
│  CacheEntry    │       │   Snapshot   │
│                │       │              │
│ request_hash PK│       │ id (UUID)    │
│ response_blob  │       │ session_id   │
│ model          │       │ label        │
│ hit_count      │       │ directory    │
│ original_cost  │       │ blob_hash ───┼──► Blob Store (tar.gz)
│ last_hit_at    │       │ file_count   │
└────────────────┘       │ size_bytes   │
                         └──────────────┘

┌──────────────────┐       ┌───────────────────┐
│   Baseline       │ 1───* │  BaselineStep     │
│                  │       │                   │
│ id (UUID)        │       │ id (UUID)         │
│ name (UNIQUE)    │       │ baseline_id       │
│ source_session_id│       │ step_number       │
│ source_timeline_id       │ step_type         │
│ step_count       │       │ expected_status   │
│ total_tokens     │       │ expected_model    │
│ description      │       │ tokens_in/out     │
│ metadata {}      │       │ tool_name         │
└──────────────────┘       │ response_blob ────┼──► Blob Store
                           │ request_blob  ────┼──► Blob Store
                           │ has_error         │
                           └───────────────────┘
```

### Session Status Lifecycle

```
  ┌──────────┐     uninit()/     ┌───────────┐
  │Recording │ ──────atexit────► │ Completed │
  │  (●)     │                   │   (✓)     │
  └────┬─────┘                   └───────────┘
       │
       │ unrecoverable error
       ▼
  ┌──────────┐     fork          ┌──────────┐
  │  Failed  │                   │  Forked  │
  │   (✗)    │                   │   (⑂)    │
  └──────────┘                   └──────────┘
```

### Timeline DAG (Self-Referencing Tree)

```
Session "research-agent"
│
├── Timeline "main" (id: t1, parent: null, fork_at: null)
│   Steps: [1, 2, 3, 4, 5(error)]
│
├── Timeline "fixed" (id: t2, parent: t1, fork_at: 4)
│   Own steps: [5(success)]
│   Full view: [1←t1, 2←t1, 3←t1, 4←t1, 5←t2]
│
└── Timeline "experiment" (id: t3, parent: t2, fork_at: 3)
    Own steps: [4, 5, 6]
    Full view: [1←t1, 2←t1, 3←t1, 4←t3, 5←t3, 6←t3]
```

**Critical design**: Forked timelines don't copy parent steps. They store only their *new* steps. The `get_full_timeline_steps()` method reconstructs the complete view by merging inherited + own steps at query time.

### SQLite Schema (DDL)

```sql
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    created_at      TEXT NOT NULL,     -- RFC 3339
    updated_at      TEXT NOT NULL,
    status          TEXT NOT NULL,     -- 'recording'|'completed'|'failed'|'forked'
    total_steps     INTEGER NOT NULL DEFAULT 0,
    total_cost_usd  REAL NOT NULL DEFAULT 0.0,
    total_tokens    INTEGER NOT NULL DEFAULT 0,
    metadata        TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE timelines (
    id                  TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id),
    parent_timeline_id  TEXT,          -- NULL for root timeline
    fork_at_step        INTEGER,       -- NULL for root timeline
    created_at          TEXT NOT NULL,
    label               TEXT NOT NULL  -- 'main', 'fixed', custom
);

CREATE TABLE steps (
    id              TEXT PRIMARY KEY,
    timeline_id     TEXT NOT NULL REFERENCES timelines(id),
    session_id      TEXT NOT NULL REFERENCES sessions(id),
    step_number     INTEGER NOT NULL,
    step_type       TEXT NOT NULL,     -- 'llm_call'|'tool_call'|'tool_result'
    status          TEXT NOT NULL,     -- 'success'|'error'|'pending'
    created_at      TEXT NOT NULL,
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    tokens_in       INTEGER NOT NULL DEFAULT 0,
    tokens_out      INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL NOT NULL DEFAULT 0.0,
    model           TEXT NOT NULL DEFAULT '',
    request_blob    TEXT NOT NULL,     -- SHA-256 hash → blob store
    response_blob   TEXT NOT NULL,     -- SHA-256 hash → blob store
    error           TEXT              -- NULL if success
);

CREATE TABLE replay_cache (
    request_hash    TEXT PRIMARY KEY,  -- SHA-256 of request body
    response_blob   TEXT NOT NULL,
    model           TEXT NOT NULL,
    tokens_in       INTEGER NOT NULL,
    tokens_out      INTEGER NOT NULL,
    original_cost_usd REAL NOT NULL,
    hit_count       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    last_hit_at     TEXT
);

CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    session_id  TEXT,
    label       TEXT NOT NULL,
    directory   TEXT NOT NULL,
    blob_hash   TEXT NOT NULL,        -- SHA-256 → tar.gz in blob store
    file_count  INTEGER NOT NULL,
    size_bytes  INTEGER NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE baselines (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL UNIQUE,
    source_session_id   TEXT NOT NULL REFERENCES sessions(id),
    source_timeline_id  TEXT NOT NULL REFERENCES timelines(id),
    created_at          TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    step_count          INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER NOT NULL DEFAULT 0,
    metadata            TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE baseline_steps (
    id              TEXT PRIMARY KEY,
    baseline_id     TEXT NOT NULL REFERENCES baselines(id) ON DELETE CASCADE,
    step_number     INTEGER NOT NULL,
    step_type       TEXT NOT NULL,
    expected_status TEXT NOT NULL,
    expected_model  TEXT NOT NULL DEFAULT '',
    tokens_in       INTEGER NOT NULL DEFAULT 0,
    tokens_out      INTEGER NOT NULL DEFAULT 0,
    tool_name       TEXT,
    response_blob   TEXT NOT NULL DEFAULT '',
    request_blob    TEXT NOT NULL DEFAULT '',
    has_error       INTEGER NOT NULL DEFAULT 0
);

-- Indexes
CREATE INDEX idx_steps_timeline ON steps(timeline_id, step_number);
CREATE INDEX idx_steps_session ON steps(session_id);
CREATE INDEX idx_timelines_session ON timelines(session_id);
CREATE UNIQUE INDEX idx_baselines_name ON baselines(name);
CREATE INDEX idx_baseline_steps_baseline ON baseline_steps(baseline_id, step_number);
```

### Pragmas (both Rust & Python)

```sql
PRAGMA journal_mode = WAL;        -- Write-Ahead Logging for concurrent reads
PRAGMA foreign_keys = ON;         -- Enforce referential integrity
PRAGMA busy_timeout = 5000;       -- Wait 5s on lock contention
```

---

## 5. Storage Layer: rewind-store

### Blob Store (Content-Addressed)

```
~/.rewind/objects/
├── 5a/
│   └── 3f9e28c1d4b7...    ← file containing request JSON
├── c7/
│   └── 8d2b4f9ea123...    ← file containing response JSON
└── ef/
    └── 12ab34cd56ef...    ← file containing snapshot tar.gz
```

**Algorithm**: Git-style content addressing.

```
Input: {"model":"gpt-4o","messages":[...]}
  │
  ▼
SHA-256 hash → "5a3f9e28c1d4b7..."
  │
  ├── Directory: objects/5a/
  └── Filename:  3f9e28c1d4b7...
  
If file exists → skip write (deduplication)
If not → write file
Return hash as reference
```

**Why content-addressed?**

1. **Deduplication**: Identical requests/responses stored once (common in agent loops)
2. **Immutability**: Blobs never modified after creation — append-only
3. **Integrity**: Hash *is* the address — corruption detectable
4. **Cheap forking**: Forked timelines share parent step blobs without copying

**Format compatibility**: Both Rust (`serde_json::to_vec`) and Python (`json.dumps(separators=(",",":")`) produce **compact JSON** with no spaces — ensuring identical hashes for identical content across implementations.

### Store API (Rust)

```rust
pub struct Store {
    conn: Connection,       // rusqlite connection
    blobs: BlobStore,       // content-addressed storage
    root: PathBuf,          // ~/.rewind/
}

impl Store {
    pub fn open(path: &Path) -> Result<Self>;           // Open + migrate
    
    // Sessions
    pub fn create_session(&self, session: &Session) -> Result<()>;
    pub fn get_session(&self, id: &str) -> Result<Option<Session>>;
    pub fn list_sessions(&self) -> Result<Vec<Session>>;
    pub fn update_session_stats(&self, id: &str, steps: u32, cost: f64, tokens: u64) -> Result<()>;
    
    // Timelines
    pub fn create_timeline(&self, timeline: &Timeline) -> Result<()>;
    pub fn get_timelines(&self, session_id: &str) -> Result<Vec<Timeline>>;
    pub fn get_root_timeline(&self, session_id: &str) -> Result<Option<Timeline>>;
    
    // Steps
    pub fn create_step(&self, step: &Step) -> Result<()>;
    pub fn get_steps(&self, timeline_id: &str) -> Result<Vec<Step>>;
    pub fn get_step(&self, step_id: &str) -> Result<Option<Step>>;
    
    // Cache
    pub fn cache_put(&self, ...) -> Result<()>;
    pub fn cache_get(&self, request_hash: &str) -> Result<Option<CacheEntry>>;
    pub fn cache_hit(&self, request_hash: &str) -> Result<()>;  // Increment counter
    pub fn cache_stats(&self) -> Result<CacheStats>;
    
    // Snapshots
    pub fn create_snapshot(&self, snapshot: &Snapshot) -> Result<()>;
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>>;
    pub fn get_snapshot(&self, ref_: &str) -> Result<Option<Snapshot>>;
    
    // Baselines
    pub fn create_baseline(&self, baseline: &Baseline) -> Result<()>;
    pub fn list_baselines(&self) -> Result<Vec<Baseline>>;
    pub fn get_baseline_by_name(&self, name: &str) -> Result<Option<Baseline>>;
    pub fn get_baseline(&self, id: &str) -> Result<Option<Baseline>>;
    pub fn delete_baseline(&self, id: &str) -> Result<()>;
    pub fn create_baseline_step(&self, step: &BaselineStep) -> Result<()>;
    pub fn get_baseline_steps(&self, baseline_id: &str) -> Result<Vec<BaselineStep>>;
    
    // Blob access
    pub fn blobs(&self) -> &BlobStore;
}
```

---

## 6. Recording Layer: rewind-proxy

### Request/Response Flow

```
Agent HTTP Request
       │
       ▼
┌──────────────────────────────────────────────────────┐
│  ProxyServer::handle_request()                        │
│                                                       │
│  1. Increment step counter (atomic)                   │
│  2. Read request body from stream                     │
│  3. Extract model from JSON body                      │
│  4. SHA-256 hash request → store as blob              │
│  5. Check: is stream:true in body?                    │
│                                                       │
│  ┌──────────────────────────────────────────────┐     │
│  │ Instant Replay Check (if enabled & !stream)  │     │
│  │                                              │     │
│  │  cache_get(request_hash)                     │     │
│  │    HIT  → return cached response ($0 cost)   │     │
│  │    MISS → continue to upstream               │     │
│  └──────────────────────────────────────────────┘     │
│                                                       │
│  6. Forward request to upstream API                    │
│                                                       │
│  ┌─────────────┐     ┌──────────────────────┐        │
│  │ Non-Stream  │     │ Streaming (SSE)      │        │
│  │             │     │                      │        │
│  │ Collect     │     │ Spawn background     │        │
│  │ full body   │     │ task to:             │        │
│  │             │     │  - Forward chunks    │        │
│  │ Parse usage │     │  - Accumulate text   │        │
│  │ Est. cost   │     │  - Parse deltas      │        │
│  │ Store blob  │     │  - Build synthetic   │        │
│  │ Create Step │     │    response          │        │
│  │ Update stats│     │  - Store blob        │        │
│  │             │     │  - Create Step       │        │
│  │ Return resp │     │  - Update stats      │        │
│  └─────────────┘     └──────────────────────┘        │
└──────────────────────────────────────────────────────┘
```

### ProxyServer State

```rust
pub struct ProxyServer {
    store: Arc<Mutex<Store>>,          // Thread-safe DB access
    session_id: String,                // Current session UUID
    timeline_id: String,               // Current timeline UUID
    step_counter: Arc<Mutex<u32>>,     // Auto-incrementing (1-indexed)
    upstream_base: String,             // e.g., "https://api.openai.com"
    instant_replay: bool,              // Cache enabled?
}
```

### SSE Event Format Detection

The proxy auto-detects whether the upstream is OpenAI or Anthropic:

```
OpenAI SSE:
  data: {"choices":[{"delta":{"content":"Hello"}}]}
  data: {"choices":[{"delta":{"content":" world"}}]}
  data: {"choices":[{"delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}
  data: [DONE]

Anthropic SSE:
  event: message_start
  data: {"type":"message_start","message":{"model":"claude-3-5-sonnet","usage":{"input_tokens":10}}}
  
  event: content_block_delta
  data: {"type":"content_block_delta","delta":{"text":"Hello"}}
  
  event: message_delta
  data: {"type":"message_delta","usage":{"output_tokens":5}}
  
  event: message_stop
```

Both formats are parsed and a **synthetic complete response** is constructed to match the non-streaming format — so the blob store always contains a uniform representation.

---

## 7. Analysis Layer: rewind-replay

### ReplayEngine

```rust
pub struct ReplayEngine<'a> {
    store: &'a Store,
}
```

### Full Timeline Reconstruction

```rust
fn get_full_timeline_steps(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Step>> {
    let timelines = self.store.get_timelines(session_id)?;
    let timeline = find(timelines, timeline_id);
    
    if let Some(parent_id) = &timeline.parent_timeline_id {
        // FORKED: inherit parent steps up to fork point
        let parent_steps = self.store.get_steps(parent_id)?;
        let inherited: Vec<Step> = parent_steps
            .into_iter()
            .filter(|s| s.step_number <= timeline.fork_at_step.unwrap())
            .collect();
        
        let own_steps = self.store.get_steps(timeline_id)?;
        
        let mut all = inherited;
        all.extend(own_steps);
        all.sort_by_key(|s| s.step_number);
        Ok(all)
    } else {
        // ROOT: just return own steps
        self.store.get_steps(timeline_id)
    }
}
```

**Note**: This is currently single-level — it doesn't recursively traverse grandparent timelines. A fork of a fork would need the parent to also be resolved. This is a known simplification.

### Timeline Diff Algorithm

```
Left Timeline:   [S1, S2, S3, S4, S5_error]
Right Timeline:  [S1, S2, S3, S4, S5_success]

Comparison:
  Step 1: Same     (response_blob equal, status equal)
  Step 2: Same
  Step 3: Same
  Step 4: Same
  Step 5: Modified (different response_blob or status)
         ▲
         └── divergence_at_step = 5

Result: TimelineDiff {
    left_id, right_id,
    total_steps_left: 5,
    total_steps_right: 5,
    divergence_at_step: Some(5),
    steps: [Same, Same, Same, Same, Modified { left_status, right_status, ... }]
}
```

### Fork Operation

```rust
fn fork(&self, session_id: &str, at_step: u32, label: &str) -> Result<Timeline> {
    let root = self.store.get_root_timeline(session_id)?;
    let fork = Timeline {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        parent_timeline_id: Some(root.id.clone()),
        fork_at_step: Some(at_step),
        label: label.to_string(),
        created_at: Utc::now(),
    };
    self.store.create_timeline(&fork)?;
    Ok(fork)
}
```

---

## 8. Assertion Layer: rewind-assert

### Purpose

Turn recorded sessions into regression tests. Create a "baseline" from a known-good session, then check new sessions against it for regressions.

### Architecture

```
rewind-assert/
├── tolerance.rs    ── Configurable check thresholds
├── extract.rs      ── Tool name + response fingerprint extraction from blobs
├── baseline.rs     ── BaselineManager: CRUD for baselines + step signature extraction
└── checker.rs      ── AssertionEngine: per-step comparison with 8 checks, 5 verdicts
```

### Baseline Creation Flow

```
Session (known-good)
       │
       ▼
BaselineManager::create_baseline(session_id, name)
       │
       ├── ReplayEngine::get_full_timeline_steps()  ← Resolves fork inheritance
       │
       ├── For each step:
       │     extract_tool_name(store, step)          ← Parses OpenAI + Anthropic JSON
       │     BaselineStep::from_step(step, tool_name)
       │
       ├── INSERT INTO baselines (...)               ← Metadata row
       └── INSERT INTO baseline_steps (...)          ← One row per expected step
```

**Key design**: Baselines are references, not copies. `BaselineStep` stores the step signature (type, model, tokens, tool name) and blob hashes — but the blobs themselves are shared with the original session via the content-addressed store.

### Assertion Check Algorithm

```
AssertionEngine::check(baseline_steps, actual_steps)
       │
       ▼
For step_number 1..max(baseline, actual):
       │
       ├── Baseline only → Missing (FAIL)
       ├── Actual only   → Extra (WARN, configurable)
       └── Both exist    → Run 8 checks:
             │
             ├── StepType:    llm_call == llm_call?           → FAIL if mismatch
             ├── Model:       gpt-4o == gpt-4o?               → FAIL or WARN (configurable)
             ├── Status:      baseline success, actual error?  → FAIL if new error
             ├── HasError:    baseline no error, actual error?  → FAIL
             ├── ToolName:    web_search == web_search?        → FAIL if mismatch
             ├── TokensIn:    within ±tolerance%?              → WARN if exceeded
             ├── TokensOut:   within ±tolerance%?              → WARN if exceeded
             └── ResponseContent: length ratio + tool names    → WARN if diverged
             │
             ▼
       StepVerdict:
         all passed, no warnings → Pass
         only warnings           → Warn
         any check failed        → Fail

Overall: PASSED if no Fail or Missing verdicts
```

### Tolerance Configuration

```rust
pub struct Tolerance {
    pub step_count_delta: u32,        // Extra steps allowed (default: 0)
    pub token_percentage: f64,        // Token drift allowed (default: 0.20 = ±20%)
    pub model_change_is_warning: bool, // Model change = warn not fail (default: false)
    pub extra_steps_is_warning: bool,  // Extra steps = warn not fail (default: true)
}
```

### Tool Name Extraction

Handles both providers by inspecting response JSON:

```
OpenAI:  response.choices[0].message.tool_calls[0].function.name
Anthropic: response.content[*] where type == "tool_use" → .name
```

Extracted once at baseline creation and cached in `baseline_steps.tool_name`.

### Python SDK Integration

```python
from rewind_agent import Assertions

# CI usage — one line
result = Assertions().check("booking-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

The Python `Assertions` class reads directly from the shared SQLite database, implementing the same comparison algorithm as the Rust checker.

---

## 9. MCP Server: rewind-mcp

### Transport & Protocol

```
Claude Code / Cursor / Windsurf
       │
       │ JSON-RPC over stdin/stdout
       ▼
┌──────────────────┐
│  rewind-mcp      │
│                  │
│  rmcp::Server    │
│  (stdio transport)
│                  │
│  Tools:          │
│  - list_sessions │
│  - show_session  │
│  - get_step_detail
│  - diff_timelines│
│  - fork_timeline │
│  - create_baseline
│  - check_baseline│
│  - list_baselines│
│  - show_baseline │
│  - delete_baseline
│  - list_snapshots│
│  - cache_stats   │
└──────────────────┘
```

### Tool Definitions (MCP Protocol)

Each tool is exposed via the `#[tool_router]` macro from the `rmcp` crate:

```rust
struct RewindMcp {
    store: Arc<Mutex<Store>>,
    #[tool_router]
    tool_router: ToolRouter<Self>,
}

#[tool(description = "List all recorded sessions")]
fn list_sessions(&self) -> Result<CallToolResult>;

#[tool(description = "Show step-by-step trace for a session")]
fn show_session(&self, session: String) -> Result<CallToolResult>;

#[tool(description = "Get full request/response for a step")]
fn get_step_detail(&self, step_id: String, include_request: bool) -> Result<CallToolResult>;

#[tool(description = "Compare two timelines side-by-side")]
fn diff_timelines(&self, session: String, left: String, right: String) -> Result<CallToolResult>;

#[tool(description = "Create a fork at a specific step")]
fn fork_timeline(&self, session: String, at_step: u32, label: String) -> Result<CallToolResult>;
```

### Session Resolution (shared across CLI & MCP)

```
Input: "latest"  →  most recent session by timestamp
Input: "a3f9"    →  prefix match against session IDs
Input: "a3f9e28c-..." → exact ID match
```

This pattern is reused for timeline resolution (`"main"` → label match, `"t2f8"` → prefix match).

---

## 10. CLI & TUI: rewind-cli / rewind-tui

### CLI Commands


| Command                 | Description             | Key Args                                     |
| ----------------------- | ----------------------- | -------------------------------------------- |
| `rewind record`         | Start proxy recording   | `--name`, `--port`, `--upstream`, `--replay` |
| `rewind sessions`       | List all sessions       | (none)                                       |
| `rewind show <ref>`     | Non-interactive trace   | Session ref (ID, prefix, "latest")           |
| `rewind inspect <ref>`  | Launch TUI inspector    | Session ref                                  |
| `rewind fork <session>` | Create timeline fork    | `--at <step>`, `--label`                     |
| `rewind diff <session>` | Compare timelines       | `<left> <right>`                             |
| `rewind snapshot <dir>` | Capture workspace state | `--label`                                    |
| `rewind restore <ref>`  | Restore snapshot        | Snapshot ref (ID, prefix, label)             |
| `rewind snapshots`      | List snapshots          | (none)                                       |
| `rewind cache`          | Show cache stats        | (none)                                       |
| `rewind assert baseline`| Create regression baseline | `<session>`, `--name`, `--description`    |
| `rewind assert check`   | Check session vs baseline | `<session>`, `--against`, `--token-tolerance` |
| `rewind assert list`    | List all baselines      | (none)                                       |
| `rewind assert show`    | Show baseline details   | `<name>`                                     |
| `rewind assert delete`  | Delete a baseline       | `<name>`                                     |
| `rewind demo`           | Seed 5-step demo        | (none)                                       |


### TUI Layout

```
┌─────────────────────────────────────────────────────────────────┐
│ ⏪ REWIND │ session-name │ 5 steps │ $0.0485 │ 1234 tokens     │
├──────────────────┬──────────────────────────────────────────────┤
│   TIMELINE (35%) │   DETAIL PANEL (65%)                        │
│                  │                                              │
│  ┌ 🧠 Step 1  ✓ │  ┌ Step Info ──────────────────────────────┐ │
│  ├ 🔧 Step 2  ✓ │  │ 🧠 LLM Call | Success | gpt-4o         │ │
│  ├ 📋 Step 3  ✓ │  │ ↓156 ↑28 tok | 320ms | $0.00062        │ │
│  ├ 🧠 Step 4  ✓ │  └────────────────────────────────────────┘ │
│  └ 🧠 Step 5  ✗ │  ┌ Request (40%) ─────────────────────────┐ │
│                  │  │ model: gpt-4o                           │ │
│                  │  │ ─── Messages (2) ───                    │ │
│                  │  │ [system] You are a research...          │ │
│                  │  │ [user] What is Tokyo's population?      │ │
│                  │  │ ─── Tools (1) ───                       │ │
│   ▲              │  │ 🔧 web_search                          │ │
│   │ keyboard     │  └────────────────────────────────────────┘ │
│   │ navigation   │  ┌ Response (60%) ────────────────────────┐ │
│   │ j/k or ↑/↓   │  │ [assistant]                            │ │
│   ▼              │  │ Tokyo's population is approximately... │ │
│                  │  │ tokens: 156↓ 28↑ = 184 total           │ │
│                  │  └────────────────────────────────────────┘ │
├──────────────────┴──────────────────────────────────────────────┤
│ ↑↓/jk Navigate   Tab Switch   Home/End First/Last   q Quit    │
└─────────────────────────────────────────────────────────────────┘
```

**TUI tech stack**: `ratatui` 0.29 + `crossterm` 0.28, raw terminal mode, 4Hz render loop.

---

## 11. Python SDK: rewind-agent

### Package Architecture

```
rewind_agent/
├── __init__.py      ── Public API surface (exports only)
├── patch.py         ── Mode switching (direct vs proxy) + lifecycle
├── recorder.py      ── Core: monkey-patching + recording (558 lines)
├── store.py         ── Pure Python Store (SQLite + blobs, format-compatible with Rust)
├── hooks.py         ── Decorators + framework adapters
└── _cli.py          ── Binary bootstrap (downloads native binary)
```

### Direct Mode: Monkey-Patching Mechanics

```python
# What init(mode="direct") does:

# 1. Create store + session + timeline
store = Store()                                    # Opens ~/.rewind/rewind.db
session_id, timeline_id = store.create_session()   # Atomic insert

# 2. Create recorder + patch SDKs
recorder = Recorder(store, session_id, timeline_id)

# Patches these 4 targets:
#   openai.resources.chat.completions.Completions.create          (sync)
#   openai.resources.chat.completions.AsyncCompletions.create     (async)
#   anthropic.resources.messages.Messages.create                  (sync)
#   anthropic.resources.messages.AsyncMessages.create             (async)

# 3. Each patched method does:
def patched_create(self_client, *args, **kwargs):
    start = time.time()
    try:
        result = original_create(self_client, *args, **kwargs)
        duration = (time.time() - start) * 1000
        
        if is_streaming(result):
            return StreamWrapper(result, recorder, kwargs, duration)
        else:
            recorder._record_call(model, request, response, duration)
            return result
    except Exception as e:
        recorder._record_call(model, request, None, duration, error=str(e))
        raise  # Never suppress the exception
```

### Stream Wrapper (accumulation pattern)

```python
class _OpenAIStreamWrapper:
    """Wraps OpenAI streaming response to intercept + record."""
    
    def __init__(self, stream, recorder, kwargs, start_time):
        self._stream = stream
        self._chunks = []           # Raw chunk accumulation
        self._content_parts = []    # Text fragments
        self._tool_calls = {}       # Tool call accumulation
        self._usage = None          # Final usage from last chunk
    
    def __iter__(self):
        return self
    
    def __next__(self):
        try:
            chunk = next(self._stream)
            self._accumulate(chunk)      # Buffer for later recording
            return chunk                  # Pass through to user
        except StopIteration:
            self._finalize()             # Record the complete call
            raise
    
    def _finalize(self):
        # Build synthetic response matching non-streaming format
        synthetic = {
            "choices": [{"message": {
                "role": "assistant",
                "content": "".join(self._content_parts),
                "tool_calls": list(self._tool_calls.values()),
            }}],
            "usage": self._usage,
            "model": self._model,
        }
        self._recorder._record_call(model, request, synthetic, duration)
```

### Hooks System (Decorators)

```python
# Step decorator — wraps any function with timing + metadata
@rewind_agent.step("search")
def search_web(query: str) -> str:
    return client.chat.completions.create(...)

# What happens:
#  1. step_start event emitted (with args preview)
#  2. Original function runs (LLM call recorded separately by monkey-patch)
#  3. step_end event emitted (with duration + result preview)

# Framework adapters
graph = rewind_agent.wrap_langgraph(graph)   # Wraps each node with @step
crew = rewind_agent.wrap_crew(crew)          # Hooks step_callback + task_callback
```

### CLI Bootstrap (_cli.py)

```
pip install rewind-agent
       │
       ▼
User runs: rewind sessions
       │
       ▼
Python entry point: rewind_agent._cli:main()
       │
       ├── Check: ~/.rewind/bin/rewind-0.3.0 exists?
       │     YES → subprocess.run([cached_binary] + argv)
       │     NO  ↓
       │
       ├── Check: ../../target/release/rewind exists? (dev mode)
       │     YES → subprocess.run([dev_binary] + argv)
       │     NO  ↓
       │
       ├── Check: which rewind (system PATH)
       │     YES → subprocess.run(["rewind"] + argv)
       │     NO  ↓
       │
       ├── Detect platform: darwin-aarch64
       ├── Download: github.com/agentoptics/rewind/releases/download/v0.3.0/rewind-v0.3.0-darwin-aarch64.tar.gz
       ├── Extract, chmod +x, cache at ~/.rewind/bin/rewind-0.3.0
       └── subprocess.run([cached_binary] + argv)
```

**Why this design?** Users `pip install rewind-agent` and get both the Python SDK and the native CLI in one step, without needing Rust toolchain.

---

## 12. Key Algorithms

### Instant Replay Cache

```
Purpose: Re-serve identical LLM requests at $0 cost.

Request comes in:
  │
  ▼
hash = SHA-256(request_body)
  │
  ├─ Cache HIT (non-streaming only):
  │    response = blobs.get(cached_entry.response_blob)
  │    Record step with cost_usd = 0.0
  │    Return cached response
  │    Log: "⚡ Instant Replay cache hit"
  │
  └─ Cache MISS:
       Forward to upstream
       Record step with actual cost
       cache_put(hash, response_blob, model, tokens, cost)
       Return upstream response

Limitations:
  - Streaming requests bypass cache (can't cache SSE stream cheaply)
  - Request body must be byte-identical (model, messages, temperature, etc.)
  - No TTL/eviction — grows unbounded (suitable for dev/debug sessions)
```

### Snapshot Pack/Restore

```
Pack: rewind snapshot ./my-project --label "before-refactor"
  │
  ├── Walk directory tree (skip: .git, target, node_modules, __pycache__)
  ├── Create tar.gz in memory
  ├── Store tar.gz blob (content-addressed)
  ├── Record Snapshot metadata (file_count, size_bytes, directory)
  └── Return snapshot ID

Restore: rewind restore before-refactor
  │
  ├── Resolve ref (by label, ID, or prefix)
  ├── Load tar.gz blob from blob store
  ├── Extract to original directory path
  └── Print restored file count
```

### Cost Estimation

```
Pricing table (per 1M tokens):
┌────────────────────────┬─────────┬──────────┐
│ Model                  │ Input   │ Output   │
├────────────────────────┼─────────┼──────────┤
│ gpt-4o                 │ $2.50   │ $10.00   │
│ gpt-4o-mini            │ $0.15   │ $0.60    │
│ gpt-4                  │ $30.00  │ $60.00   │
│ gpt-3.5-turbo          │ $0.50   │ $1.50    │
│ claude-3-5-sonnet      │ $3.00   │ $15.00   │
│ claude-sonnet-4        │ $3.00   │ $15.00   │
│ claude-3-5-haiku       │ $0.80   │ $4.00    │
│ claude-haiku-4         │ $0.80   │ $4.00    │
│ claude-opus            │ $15.00  │ $75.00   │
│ Default (unknown)      │ $3.00   │ $15.00   │
└────────────────────────┴─────────┴──────────┘

Formula: cost = (tokens_in * input_rate + tokens_out * output_rate) / 1_000_000

Model matching: prefix-based (e.g., "gpt-4o-2024-08-06" matches "gpt-4o")
```

---

## 13. Concurrency & Thread Safety

### Rust Side

```
ProxyServer:
  store: Arc<Mutex<Store>>        ← Shared across all request handlers
  step_counter: Arc<Mutex<u32>>   ← Atomic increment per request

Pattern:
  - Each HTTP connection spawned as tokio::task::spawn()
  - Store lock acquired briefly for each DB write
  - Step counter lock held only during increment (not during API call)
  - Streaming: background task accumulates, main task forwards chunks
    via tokio::sync::mpsc::channel()
```

### Python Side

```python
class Recorder:
    _lock = threading.Lock()       # Protects counter + DB writes
    _step_counter: int = 0

    def _record_call(self, ...):
        with self._lock:           # Atomic block
            self._step_counter += 1
            step_number = self._step_counter
            self._store.create_step(...)
            self._store.update_session_stats(...)
```

**Key invariant**: Step numbers are strictly sequential and unique within a timeline. The lock ensures no gaps or duplicates even under concurrent writes (tested with 50-100 concurrent threads).

### WAL Mode Benefits

```
Writer (Recorder)              Reader (CLI/TUI/MCP)
     │                              │
     │ BEGIN                        │ SELECT * FROM steps
     │ INSERT INTO steps            │   (reads from WAL snapshot)
     │ COMMIT                       │   (no blocking!)
     │                              │
```

WAL mode allows concurrent readers while a single writer commits. This means the TUI can inspect a session while the proxy is actively recording — no lock contention.

---

## 14. Streaming Architecture

### The Streaming Problem

When an LLM API returns `stream: true`, the response arrives as Server-Sent Events (SSE). Rewind must:

1. **Forward chunks to the caller in real-time** (no buffering delay)
2. **Accumulate the full response** for storage
3. **Build a synthetic non-streaming response** for uniform blob format

### Proxy Solution (Rust)

```
Upstream API (SSE)           Proxy                    Client (Agent)
     │                        │                           │
     │ data: {"delta":"Hi"}   │                           │
     │ ──────────────────────►│                           │
     │                        │ Forward chunk ────────────► 
     │                        │ Accumulate "Hi"           │
     │                        │                           │
     │ data: {"delta":" w"}   │                           │
     │ ──────────────────────►│                           │
     │                        │ Forward chunk ────────────►
     │                        │ Accumulate " w"           │
     │                        │                           │
     │ data: [DONE]           │                           │
     │ ──────────────────────►│                           │
     │                        │ Forward [DONE] ───────────►
     │                        │                           │
     │                        │ Build synthetic response: │
     │                        │ {"choices":[{"message":   │
     │                        │   {"content":"Hi w"}}]}   │
     │                        │                           │
     │                        │ Store blob + Step record  │
     └                        └                           └

Implementation: tokio::sync::mpsc::channel()
  - Background task: reads upstream, accumulates, sends to channel
  - Foreground: reads from channel, forwards to client
  - On completion: background task finalizes recording
```

### SDK Solution (Python)

```python
# Wrapper intercepts the iterator/async iterator
for chunk in stream_wrapper:     # User iterates normally
    print(chunk.choices[0].delta.content)  # They see each chunk
    # Internally: wrapper accumulates content + tool_calls

# When iteration ends (StopIteration), wrapper calls _finalize()
# which builds synthetic response and records it
```

---

## 15. Cost Tracking

### Per-Step

Each `Step` record stores:

- `tokens_in` / `tokens_out` — extracted from API response usage
- `cost_usd` — estimated from model pricing table
- `model` — the actual model string from the response

### Per-Session (Aggregated)

```sql
-- Updated atomically after each step
UPDATE sessions SET
    total_steps = total_steps + 1,
    total_cost_usd = total_cost_usd + ?,
    total_tokens = total_tokens + ?
WHERE id = ?;
```

### Instant Replay Savings

```
Cache stats track:
  - entries: number of cached request/response pairs
  - total_hits: how many times a cached response was served
  - total_saved_usd: sum of original_cost_usd for all cache hits

Every cache hit has cost_usd = 0.0 in the step record,
while the original_cost_usd is accumulated in the savings counter.
```

---

## 16. CI/CD & Distribution

### CI Pipeline (`.github/workflows/ci.yml`)

```
Trigger: push to master/main, PRs

Jobs:
  build (matrix: ubuntu, macos):
    - Install Rust stable + clippy
    - cargo build --release
    - cargo clippy -- -D warnings
    - Integration: rewind demo → rewind sessions → rewind show latest

  python (ubuntu):
    - Python 3.12
    - pip install -e python/
    - Import verification for all public API symbols
```

### Release Pipeline (`.github/workflows/release.yml`)

```
Trigger: git tag v*

Build Matrix:
  ┌──────────────┬────────────────────────────────┐
  │ Target       │ Runner                         │
  ├──────────────┼────────────────────────────────┤
  │ x86_64-linux │ ubuntu-latest                  │
  │ aarch64-linux│ ubuntu-latest + cross-compile  │
  │ x86_64-macos │ macos-latest                   │
  │ aarch64-macos│ macos-latest                   │
  └──────────────┴────────────────────────────────┘

Artifacts per target:
  rewind-v{tag}-{target}.tar.gz
  rewind-v{tag}-{target}.tar.gz.sha256

Upload: GitHub Release with auto-generated notes
```

### Distribution Channels

```
┌─────────────┬────────────────────────────────────────────┐
│ Channel     │ What it delivers                           │
├─────────────┼────────────────────────────────────────────┤
│ PyPI        │ Python SDK + auto-downloads native binary  │
│ GitHub      │ Pre-built binaries for 4 platforms         │
│ curl        │ Install script for macOS/Linux             │
│ Cargo       │ Build from source                          │
└─────────────┴────────────────────────────────────────────┘
```

---

## 17. Design Decisions & Trade-offs

### Why SQLite + Blob Store (not Postgres, not flat files)?

**Decision**: Single-file SQLite database + git-style content-addressed blob store.

**Rationale**:

- **Zero setup**: No database server to install/configure
- **Portable**: `~/.rewind/` directory is self-contained, can be copied/backed up
- **WAL mode**: Concurrent reads during active recording (TUI can inspect while proxy records)
- **Blob separation**: Large request/response payloads don't bloat the database. SQLite stores metadata (fast queries), blobs store payloads (efficient dedup).
- **Content addressing**: Identical LLM calls (common in agent retry loops) stored once

**Trade-off**: No multi-machine access, no built-in replication. Acceptable for a developer debugging tool.

### Why Monkey-Patching (not middleware, not wrapper)?

**Decision**: Direct mode patches `openai.resources.chat.completions.Completions.create` at runtime.

**Rationale**:

- **Zero code changes**: User's agent code doesn't need to change
- **One-line setup**: `rewind_agent.init()` — that's it
- **Framework agnostic**: Works with any framework that uses OpenAI/Anthropic SDKs
- **Streaming transparent**: Wrapper iterator is duck-type compatible

**Trade-off**: Fragile to SDK internal API changes. Mitigated by targeting stable, versioned entry points.

### Why Two Recording Modes?

**Decision**: HTTP proxy mode (language-agnostic) + Python SDK direct mode (zero-latency).

**Rationale**:

- **Proxy**: Works with any language (JS, Go, Rust agents). No SDK dependency.
- **Direct**: Zero latency overhead, no port management, simpler setup for Python users.
- **Shared format**: Both write identical SQLite + blob store format — all inspection tools work with either.

### Why Content-Addressed Blobs (not inline JSON)?

**Decision**: Request/response stored as SHA-256-keyed files, referenced by hash from `steps` table.

**Rationale**:

- **Deduplication**: System prompts repeated across every call → stored once
- **Database stays small**: `steps` table has fixed-width columns, no multi-MB JSON blobs
- **Lazy loading**: TUI/CLI can list steps without loading payloads
- **Integrity**: Hash = address means corruption is detectable

### Why Synthetic Response for Streaming?

**Decision**: Accumulate streaming chunks and build a fake non-streaming response for storage.

**Rationale**:

- **Uniform format**: All blobs have the same JSON shape regardless of streaming
- **Queryable**: Can extract model, tokens, content without parsing SSE
- **Diff-friendly**: Timeline diff compares response blobs directly

**Trade-off**: Raw SSE data is lost (only synthetic stored). Could add raw SSE blob as secondary if needed.

### Why Fork-at-Step (not Fork-at-Token)?

**Decision**: Forking granularity is at the step (LLM call) level, not mid-response.

**Rationale**:

- **Clean semantics**: Each step is an atomic unit (request → response)
- **Cheap implementation**: Fork = new timeline row + parent reference
- **Sufficient for debugging**: "What if step 4 returned different data?" is the primary question

---

## 18. Roadmap & Known Gaps

### Current Gaps


| Gap                                      | Impact                                           | Status                         |
| ---------------------------------------- | ------------------------------------------------ | ------------------------------ |
| Annotations not persisted in direct mode | `@step`/`annotate()` data lost on session end    | In-memory only, warning logged |
| Instant Replay is proxy-only             | Direct mode can't cache                          | Proxy feature only             |
| Single-level fork inheritance            | Fork-of-fork doesn't recurse through grandparent | Known simplification           |
| No TTL/eviction on cache                 | Cache grows unbounded                            | OK for debug sessions          |
| Python SDK version mismatch              | pyproject.toml says 0.2.0, some refs say 0.3.0   | Version bump in progress       |


### Roadmap


| Version  | Features                                                                                  |
| -------- | ----------------------------------------------------------------------------------------- |
| **v0.2** | Web UI (browser-based session viewer), Fork-and-execute (re-run from fork point)          |
| **v1.0** | Live breakpoints (pause agent mid-execution), Cloud (shared sessions, team collaboration) |


---

## Quick Reference: File Sizes


| Component        | Lines      | Files               |
| ---------------- | ---------- | ------------------- |
| rewind-store     | ~620       | 4 (.rs)             |
| rewind-proxy     | ~600       | 2 (.rs)             |
| rewind-replay    | ~200       | 1 (.rs)             |
| rewind-assert    | ~690       | 5 (.rs)             |
| rewind-mcp       | ~650       | 2 (.rs)             |
| rewind-cli       | ~1,000     | 1 (.rs)             |
| rewind-tui       | ~500       | 1 (.rs)             |
| **Rust total**   | **~4,260** | **16**              |
| Python SDK       | ~1,580     | 7 (.py)             |
| Python tests     | ~680       | 3 (.py)             |
| **Python total** | **~2,260** | **10**              |
| **Grand total**  | **~6,520** | **26 source files** |


