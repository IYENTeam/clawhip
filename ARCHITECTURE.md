# Claw OS Architecture — clawhip v0.3.0+

> clawhip evolves from a Discord notification router into **Claw OS** — the operating system for AI development teams.

## Design Principles

1. **Everything is a plugin** — no hardcoded integrations
2. **Events are the universal language** — all data flows as typed events
3. **Providers are swappable** — Discord today, Slack tomorrow, both next week
4. **Local-first** — daemon runs on your machine, no cloud dependency
5. **Zero-config defaults** — works out of the box, scales with config

## Current Architecture (v0.2.0)

```
┌─────────────────────────────────────────────┐
│                  clawhip daemon              │
│                                              │
│  CLI ──→ HTTP API ──→ Router ──→ Discord     │
│                         ↑                    │
│  Git Monitor ───────────┤                    │
│  tmux Monitor ──────────┤                    │
│  GitHub Webhooks ───────┘                    │
└─────────────────────────────────────────────┘
```

**Problems:**
- `DiscordClient` is hardcoded everywhere
- Router returns `DeliveryTarget::Channel | Webhook` — Discord-specific
- Monitor dispatches directly to Discord
- No way to add Slack/Notion without duplicating everything
- Events are stringly-typed (`kind: String`)
- No project/session state persistence

## Target Architecture (v0.3.0 — Claw OS)

```
┌──────────────────────────────────────────────────────────────────┐
│                         clawhip daemon                           │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌───────────────────┐  │
│  │ Event Sources │───→│  Event Bus   │───→│ Channel Providers │  │
│  └──────────────┘    └──────┬───────┘    └───────────────────┘  │
│                              │                                   │
│  ┌──────────────┐    ┌──────┴───────┐    ┌───────────────────┐  │
│  │   Inbound    │───→│    Router    │───→│    Outbound       │  │
│  │  Providers   │    │  (rules +    │    │   Providers       │  │
│  │              │    │   filters)   │    │                   │  │
│  └──────────────┘    └──────────────┘    └───────────────────┘  │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌───────────────────┐  │
│  │ Session Mgr  │    │ Project Store│    │  Agent Runtime    │  │
│  └──────────────┘    └──────────────┘    └───────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

## Core Abstractions

### 1. Event (`crate::event::Event`)

All data flows as strongly-typed events with a common envelope:

```rust
pub struct Event {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: EventSource,
    pub kind: EventKind,
    pub payload: Value,
    pub metadata: EventMetadata,
}

pub enum EventSource {
    Git { repo: String },
    GitHub { repo: String },
    Tmux { session: String, pane: String },
    Agent { session: String, engine: AgentEngine },
    Inbound { provider: String },
    System,
}

pub enum EventKind {
    // Git
    GitCommit,
    GitBranchChanged,
    GitPushAggregated,
    
    // GitHub
    GitHubIssueOpened,
    GitHubPROpened,
    GitHubPRMerged,
    GitHubPRStatusChanged,
    GitHubCIFailed,
    
    // tmux
    TmuxKeyword,
    TmuxStale,
    
    // Agent lifecycle
    AgentStarted,
    AgentBlocked,
    AgentFinished,
    AgentFailed,
    
    // Session management
    SessionCreated,
    SessionCompleted,
    SessionStale,
    
    // Custom
    Custom(String),
}

pub struct EventMetadata {
    pub project: Option<String>,
    pub channel_hint: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
    pub priority: EventPriority,
}

pub enum EventPriority {
    Low,      // routine updates
    Normal,   // standard notifications
    High,     // failures, blockers
    Critical, // system down, data loss
}
```

### 2. Provider Trait (`crate::provider::Provider`)

All external integrations implement this trait:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// Unique provider name (e.g. "discord", "slack", "notion")
    fn name(&self) -> &str;

    /// Provider capabilities
    fn capabilities(&self) -> ProviderCapabilities;

    /// Initialize from config
    async fn init(config: &ProviderConfig) -> Result<Self> where Self: Sized;

    /// Send a message/notification (outbound)
    async fn send(&self, target: &str, message: &RenderedMessage) -> Result<()>;

    /// Format an event for this provider
    fn render(&self, event: &Event, format: &MessageFormat) -> Result<RenderedMessage>;
}

pub struct ProviderCapabilities {
    pub outbound: bool,           // can send messages
    pub inbound: bool,            // can receive events (webhooks)
    pub reactions: bool,          // supports emoji reactions
    pub threads: bool,            // supports threaded replies
    pub rich_formatting: bool,    // supports embeds/blocks
    pub bidirectional_sync: bool, // can sync state (Notion/Jira)
}

pub struct RenderedMessage {
    pub text: String,
    pub rich: Option<Value>,  // provider-specific rich format (embeds, blocks)
}
```

### 3. Event Source Trait (`crate::source::EventSource`)

Monitors that produce events:

```rust
#[async_trait]
pub trait EventSource: Send + Sync {
    fn name(&self) -> &str;
    
    /// Start polling/watching and emit events through the bus
    async fn run(&self, bus: EventBus) -> Result<()>;
    
    /// One-shot check (for CLI commands)
    async fn check(&self) -> Result<Vec<Event>>;
}
```

**Built-in sources:**
- `GitSource` — local git repo polling (commits, branches)
- `GitHubSource` — GitHub API polling (PRs, issues, CI)
- `TmuxSource` — tmux pane monitoring (keywords, stale)
- `AgentSource` — OMC/OMX lifecycle events
- `WebhookSource` — inbound HTTP webhooks

### 4. Router (`crate::router::Router`)

Event → Provider routing with filter chains:

```rust
pub struct RouteRule {
    pub event_pattern: String,         // glob: "github.*", "tmux.stale"
    pub filters: BTreeMap<String, String>,  // payload field matching
    pub provider: String,              // "discord", "slack", "notion"  
    pub target: String,                // channel ID, webhook URL, database ID
    pub format: Option<MessageFormat>,
    pub mention: Option<String>,
    pub template: Option<String>,
    pub priority_override: Option<EventPriority>,
    pub transform: Option<String>,     // future: event transformation
}
```

**Routing flow:**
```
Event → match rules → select provider → render message → deliver
         ↓ (no match)
    default provider + default channel
```

**Multi-provider routing** — one event can match multiple rules:
```toml
# Same event → Discord + Slack
[[routes]]
event = "github.ci-failed"
provider = "discord"
target = "1234567890"
format = "alert"

[[routes]]
event = "github.ci-failed"
provider = "slack"
target = "#ci-alerts"
format = "alert"
```

### 5. Event Bus (`crate::bus::EventBus`)

Central pub/sub for decoupling sources from consumers:

```rust
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn emit(&self, event: Event);
    pub fn subscribe(&self) -> broadcast::Receiver<Event>;
}
```

### 6. Session Manager (`crate::session::SessionManager`)

Track and orchestrate coding sessions:

```rust
pub struct SessionManager {
    sessions: HashMap<String, SessionState>,
    store: Box<dyn SessionStore>,
}

pub struct SessionState {
    pub name: String,
    pub engine: AgentEngine,       // OMC, OMX
    pub worktree: PathBuf,
    pub branch: String,
    pub issue: Option<u64>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

pub enum SessionStatus {
    Running,
    Idle,
    Stale,
    Completed { pr: Option<u64> },
    Failed { error: String },
}

pub enum AgentEngine {
    OMC,
    OMX,
    Custom(String),
}
```

### 7. Project Store (`crate::project::ProjectStore`)

Persistent project context memory:

```rust
pub struct ProjectStore {
    base_path: PathBuf,  // ~/.clawhip/projects/
}

pub struct ProjectContext {
    pub name: String,
    pub repo: String,
    pub sessions: Vec<SessionSummary>,
    pub issues: Vec<IssueSummary>,
    pub notes: Vec<ProjectNote>,
    pub last_updated: DateTime<Utc>,
}
```

Stored as local markdown/JSON — can sync to Obsidian (#32).

## Config Evolution

```toml
[daemon]
port = 25294

# Provider configs
[providers.discord]
token = "..."
default_channel = "1234567890"

[providers.slack]
webhook_url = "https://hooks.slack.com/..."
# or
token = "xoxb-..."
default_channel = "#general"

[providers.notion]
token = "secret_..."
database_id = "..."

# Event sources
[sources.git]
poll_interval_secs = 60

[[sources.git.repos]]
path = "/home/user/project"
github_repo = "org/repo"

[sources.tmux]
poll_interval_secs = 30

[[sources.tmux.sessions]]
session = "issue-*"
keywords = ["panic", "SIGKILL"]
stale_minutes = 30
keyword_window_secs = 30

[sources.agent]
engines = ["omc", "omx"]

# Routes (unchanged syntax, new `provider` field)
[[routes]]
event = "github.*"
provider = "discord"
target = "1234567890"
mention = "<@bot>"

[[routes]]
event = "github.ci-failed"
provider = "slack"
target = "#ci-alerts"

[[routes]]
event = "github.issues.opened"
provider = "notion"
target = "database-id"
transform = "issue_to_page"
```

## Migration Path (v0.2 → v0.3)

1. **Config backward compat** — old `[discord]` section still works, auto-mapped to `[providers.discord]`
2. **Routes without `provider`** — default to `discord` (current behavior)
3. **`monitors.*` → `sources.*`** — alias old key names
4. **Existing `DiscordClient`** → implements `Provider` trait
5. **No breaking CLI changes** — all new features are additive

## Implementation Phases

### Phase 1: Core Abstractions (0.3.0-alpha)
- [ ] `Event` type + `EventKind` enum (replace stringly-typed events)
- [ ] `Provider` trait + Discord as first implementation
- [ ] `EventBus` (broadcast channel)
- [ ] `EventSource` trait + refactor Git/tmux monitors
- [ ] Config migration layer (backward compat)

### Phase 2: Multi-Provider (0.3.0-beta)
- [ ] Slack provider (#28)
- [ ] `provider` field in route config
- [ ] Multi-route delivery (one event → multiple providers)
- [ ] Provider-specific message rendering

### Phase 3: Session + State (0.3.0)
- [ ] `SessionManager` — track active coding sessions
- [ ] `ProjectStore` — persistent project context
- [ ] Agent lifecycle events (OMC/OMX)
- [ ] Context-aware keyword filtering (#39)

### Phase 4: Bidirectional Sync (0.4.0+)
- [ ] Notion provider (#29) — issue ↔ page sync
- [ ] Jira provider (#31) — issue ↔ ticket sync
- [ ] Obsidian sync (#32) — markdown export
- [ ] Inbound event processing (Notion → GitHub)

### Phase 5: Orchestration (0.5.0+)
- [ ] Work queue — issue → session auto-assignment
- [ ] Session auto-spawn from events
- [ ] Dashboard (TUI or web)
- [ ] OpenClaw/Clawdbot deep integration

## File Structure (Target)

```
src/
├── main.rs
├── cli.rs
├── config.rs              # unified config with migration
├── daemon.rs              # HTTP server + lifecycle
│
├── event/
│   ├── mod.rs             # Event, EventKind, EventSource
│   ├── bus.rs             # EventBus (broadcast)
│   └── render.rs          # default rendering
│
├── provider/
│   ├── mod.rs             # Provider trait
│   ├── discord.rs         # Discord provider
│   ├── slack.rs           # Slack provider
│   ├── notion.rs          # Notion provider (future)
│   └── webhook.rs         # Generic webhook provider
│
├── source/
│   ├── mod.rs             # EventSource trait
│   ├── git.rs             # Git repo monitor
│   ├── github.rs          # GitHub API monitor
│   ├── tmux.rs            # tmux pane monitor
│   ├── agent.rs           # Agent lifecycle
│   └── inbound.rs         # Inbound webhook receiver
│
├── router/
│   ├── mod.rs             # Router + RouteRule
│   ├── filter.rs          # Event filtering/matching
│   └── template.rs        # Dynamic tokens + templates
│
├── session/
│   ├── mod.rs             # SessionManager
│   └── store.rs           # Persistent session state
│
├── project/
│   ├── mod.rs             # ProjectStore
│   └── sync.rs            # Obsidian/external sync
│
└── util/
    ├── dynamic_tokens.rs
    └── keyword_window.rs
```

## Key Decisions

1. **Trait objects vs enums for providers** → Trait objects (`Box<dyn Provider>`). Enables runtime plugin loading later. Performance is irrelevant (network I/O dominates).

2. **Event bus implementation** → `tokio::sync::broadcast`. Simple, proven, no external deps. Switch to something heavier only if needed.

3. **Config format** → Stay with TOML. Add `[providers.*]` sections. Old format auto-migrated.

4. **State persistence** → JSON files in `~/.clawhip/state/`. No database. SQLite only if query patterns demand it.

5. **Plugin loading** → Compile-time for now (feature flags). Runtime loading (dylib/WASM) is Phase 5+.

---

*This is a living document. Updated as implementation progresses.*

—
*[repo owner's gaebal-gajae (clawdbot) 🦞]*
