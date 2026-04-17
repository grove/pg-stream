# PLAN_TUI_PART_3.md — TUI Dog-Feeding Integration & Architecture (v0.21)

**Milestone:** v0.21.0
**Status:** Planned
**Effort:** ~3–4 weeks
**Last updated:** 2026-04-17
**Crate:** `pgtrickle-tui`
**Depends on:** [PLAN_TUI.md](PLAN_TUI.md) (T1–T8 implemented),
  [PLAN_TUI_PART_2.md](PLAN_TUI_PART_2.md) (T9–T14 implemented)

---

## Overview

PLAN_TUI.md (v0.14) delivered 14 views, 18 CLI subcommands, and real-time
alerts. PLAN_TUI_PART_2 (v0.15) added write actions, command palette,
scrollable views, parallel polling, and 15 new SQL API surface expansions.
This third plan wires the v0.20.0 **dog-feeding stream tables** (`df_*`)
into the TUI, introduces a **structural refactoring** of the TUI internals,
and adds CLI subcommands for dog-feeding lifecycle management.

### Release Theme

> **Live, reactive visibility into pg_trickle monitoring itself.**
> Operators gain anomaly detection, CDC buffer trend visualization,
> scheduling interference analysis, and efficiency dashboards — all fed
> from the `df_*` stream tables. The TUI architecture evolves from a
> monolithic `AppState` + flat view list into domain-scoped state with
> subscription-based selective polling, preparing for sustainable growth
> beyond 20 views.

### What This Plan Covers

**Architecture improvements:**

1. **AppState domain decomposition** — split 50+ fields into 7 domain structs
2. **Subscription-based selective polling** — only fetch Phase 2 data for the active view
3. **Poller logic extraction** — separate query fetching from state mutation for testability
4. **CLI/TUI command unification** — single canonical command impl shared by both paths

**Dog-feeding TUI views (from ROADMAP v0.21.0):**

5. **TUI-1** — New Anomaly Detection view (key `a`)
6. **TUI-2** — Dashboard ribbon anomaly badge
7. **TUI-3** — CDC Health sparkline column from `df_cdc_buffer_trends`
8. **TUI-4** — CDC Health spill-risk badge
9. **TUI-5** — Workers Interference sub-tab from `df_scheduling_interference`
10. **TUI-6** — Workers scheduler overhead bar
11. **TUI-7** — Dependencies Mermaid/DOT export overlay
12. **TUI-8** — Header bar dog-feeding status badge
13. **TUI-9** — Command palette `dog-feeding enable/disable`
14. **TUI-10** — Detail view active anomaly summary row
15. **TUI-11** — Refresh Log `[auto]` annotation for `DOG_FEED` rows
16. **TUI-12** — First-launch dog-feeding setup hint toast
17. **TUI-13** — Anomaly signals as Issues
18. **TUI-14** — `dog_feed_anomaly` alert styling
19. **TUI-15** — Dashboard snapshot tests
20. **TUI-16** — Diagnostics `df_efficiency_rolling` summary panel

**Backend enhancements:**

21. **DF-21** — `sla_breach_risk` column in `df_threshold_advice`
22. **DF-22** — `dog_feeding_auto_apply = 'full'` scheduling mode
23. **DF-23** — `dog_feeding_status()` retention warning
24. **DF-24** — `recommend_refresh_mode()` reads from `df_threshold_advice`

**CLI integration:**

25. **CLI-1** — `pgtrickle dog-feeding` subcommand group
26. **CLI-2** — `pgtrickle graph --format ascii|mermaid|dot`

**Test coverage, documentation:**

27. **TEST-21** — Proptest for `df_threshold_advice` bounds
28. **TUI-T1** — Snapshot tests for all new TUI views
29. **TUI-15** — Dashboard snapshot tests (all rendering branches)
30. **TUI-D1, DOC-21, DOC-22** — Documentation updates

### What Is NOT in Scope

- ViewRenderer trait adoption for existing views — deferred to v0.22.0+
- Full navigation redesign (menu/tabs) — deferred
- Web UI — out of scope
- Multi-database switching — out of scope
- Batch multi-select operations (F17) — deferred

---

## Table of Contents

- [Current State Analysis](#current-state-analysis)
- [Architecture: Domain Decomposition](#architecture-domain-decomposition)
- [Architecture: Selective Polling](#architecture-selective-polling)
- [Architecture: Poller Extraction](#architecture-poller-extraction)
- [Architecture: CLI/TUI Command Unification](#architecture-clitui-command-unification)
- [Dog-Feeding Data Model](#dog-feeding-data-model)
- [Feature Specifications](#feature-specifications)
  - [TUI-1 — Anomaly Detection View](#tui-1--anomaly-detection-view)
  - [TUI-2 — Dashboard Anomaly Badge](#tui-2--dashboard-anomaly-badge)
  - [TUI-3 — CDC Health Sparkline Column](#tui-3--cdc-health-sparkline-column)
  - [TUI-4 — CDC Health Spill-Risk Badge](#tui-4--cdc-health-spill-risk-badge)
  - [TUI-5 — Workers Interference Sub-Tab](#tui-5--workers-interference-sub-tab)
  - [TUI-6 — Workers Scheduler Overhead Bar](#tui-6--workers-scheduler-overhead-bar)
  - [TUI-7 — Dependencies Mermaid/DOT Export](#tui-7--dependencies-mermaiddot-export)
  - [TUI-8 — Header Dog-Feeding Status Badge](#tui-8--header-dog-feeding-status-badge)
  - [TUI-9 — Dog-Feeding Commands in Palette](#tui-9--dog-feeding-commands-in-palette)
  - [TUI-10 — Detail View Anomaly Summary](#tui-10--detail-view-anomaly-summary)
  - [TUI-11 — Refresh Log Auto Tag](#tui-11--refresh-log-auto-tag)
  - [TUI-12 — First-Launch Dog-Feeding Toast](#tui-12--first-launch-dog-feeding-toast)
  - [TUI-13 — Anomaly Signals as Issues](#tui-13--anomaly-signals-as-issues)
  - [TUI-14 — Dog-Feed Alert Styling](#tui-14--dog-feed-alert-styling)
  - [TUI-15 — Dashboard Snapshot Tests](#tui-15--dashboard-snapshot-tests)
  - [TUI-16 — Diagnostics Efficiency Summary](#tui-16--diagnostics-efficiency-summary)
- [Backend Enhancements](#backend-enhancements)
  - [DF-21 — sla_breach_risk Column](#df-21--sla_breach_risk-column)
  - [DF-22 — auto_apply Full Mode](#df-22--auto_apply-full-mode)
  - [DF-23 — Retention Warning](#df-23--retention-warning)
  - [DF-24 — recommend_refresh_mode Reads DF](#df-24--recommend_refresh_mode-reads-df)
- [CLI Integration](#cli-integration)
  - [CLI-1 — Dog-Feeding Subcommand Group](#cli-1--dog-feeding-subcommand-group)
  - [CLI-2 — Graph Format Flag](#cli-2--graph-format-flag)
- [Test Strategy](#test-strategy)
- [Implementation Phases](#implementation-phases)
- [Exit Criteria](#exit-criteria)

---

## Current State Analysis

### AppState Complexity

`state.rs` defines a single `AppState` struct with 50+ fields. All views
receive the entire state. Adding dog-feeding data (5 stream table views,
status, anomaly signals, trends, interference, efficiency, threshold advice)
would push AppState past 60 fields with no clear ownership.

**Fields today (grouped by domain):**

| Domain | Field Count | Examples |
|--------|-------------|---------|
| Stream tables | 6 | `stream_tables`, `sparkline_data`, `refresh_history_cache`, `source_detail_cache`, `auxiliary_columns_cache`, `change_activity_cache` |
| CDC | 4 | `cdc_buffers`, `cdc_health`, `source_gates`, `trigger_inventory` |
| Diagnostics | 5 | `diagnostics`, `efficiency`, `scheduler_overhead`, `diag_signals`, `dedup_stats` |
| Monitoring | 4 | `health_checks`, `alerts`, `issues`, `quick_health` |
| Scheduling | 4 | `workers`, `job_queue`, `dag_edges`, `fuses` |
| Watermarks | 2 | `watermark_groups`, `watermark_alignment` |
| Config | 1 | `guc_params` |
| Caches | 4 | `delta_sql_cache`, `ddl_cache`, `diagnosed_errors`, `explain_mode_cache` |
| Shared buffer | 2 | `shared_buffer_stats`, `diamond_groups`, `scc_groups` |
| Connection | 5 | `last_poll`, `connected`, `reconnecting`, `poll_interval_ms`, `error_message`, `poll_failure_count`, `poll_total_count` |

### Polling Waste

`poller.rs` runs 21 queries every poll cycle via `tokio::join!`. Phase 2
queries (9 total) always execute regardless of the active view. On the
Dashboard, data for Watermarks alignment, SCC status, shared buffer stats,
and CDC health is fetched and discarded.

### View Organization

14 views mapped to flat key bindings (`1`–`9`, `w`, `f`, `m`, `d`, `i`).
Adding an Anomaly view (`a`) is straightforward but pushes the keyspace
further. No grouping, no parent-child relationships.

### CLI/TUI Command Divergence

CLI commands in `commands/*.rs` and TUI palette commands in `app.rs` execute
the same SQL but through different code paths. Adding `dog-feeding enable`
requires changes in both places.

---

## Architecture: Domain Decomposition

### Goal

Split `AppState` into domain-scoped sub-structs. Views access only the
domains they need. New domains (dog-feeding) are added without touching
existing view code.

### Design

```rust
// state.rs — after refactoring

/// Central state store for the TUI — updated by async pollers.
#[derive(Default)]
pub struct AppState {
    // Domain-scoped sub-states
    pub stream_tables: StreamTableDomain,
    pub cdc: CdcDomain,
    pub diagnostics: DiagnosticsDomain,
    pub monitoring: MonitoringDomain,
    pub scheduling: SchedulingDomain,
    pub watermarks: WatermarkDomain,
    pub config: ConfigDomain,
    pub dog_feeding: DogFeedingDomain,  // NEW

    // Caches (shared across views)
    pub caches: CacheStore,

    // Connection & poll health
    pub connection: ConnectionState,
}

#[derive(Default)]
pub struct StreamTableDomain {
    pub tables: Vec<StreamTableInfo>,
    pub sparkline_data: HashMap<String, Vec<f64>>,
}

#[derive(Default)]
pub struct CdcDomain {
    pub buffers: Vec<CdcBuffer>,
    pub health: Vec<CdcHealthEntry>,
    pub source_gates: Vec<SourceGate>,
    pub trigger_inventory: Vec<TriggerInfo>,
    pub shared_buffer_stats: Vec<SharedBufferInfo>,
}

#[derive(Default)]
pub struct DiagnosticsDomain {
    pub recommendations: Vec<DiagRecommendation>,
    pub efficiency: Vec<RefreshEfficiency>,
    pub scheduler_overhead: Option<SchedulerOverhead>,
    pub signals: HashMap<String, serde_json::Value>,
    pub dedup_stats: Option<DedupStats>,
}

#[derive(Default)]
pub struct MonitoringDomain {
    pub health_checks: Vec<HealthCheck>,
    pub alerts: Vec<AlertEvent>,
    pub issues: Vec<Issue>,
    pub quick_health: Option<QuickHealth>,
}

#[derive(Default)]
pub struct SchedulingDomain {
    pub workers: Option<WorkerInfo>,
    pub job_queue: Vec<JobQueueEntry>,
    pub dag_edges: Vec<DagEdge>,
    pub fuses: Vec<FuseInfo>,
    pub diamond_groups: Vec<DiamondGroup>,
    pub scc_groups: Vec<SccGroup>,
}

#[derive(Default)]
pub struct WatermarkDomain {
    pub groups: Vec<WatermarkGroup>,
    pub alignment: Vec<WatermarkAlignment>,
}

#[derive(Default)]
pub struct ConfigDomain {
    pub guc_params: Vec<GucParam>,
}

/// Dog-feeding domain — data from the five df_* stream tables.
#[derive(Default)]
pub struct DogFeedingDomain {
    /// Whether dog-feeding is set up and active
    pub enabled: bool,
    /// Dog-feeding status (active STs, setup date, health)
    pub status: Option<DogFeedingStatus>,
    /// Anomaly signals from df_anomaly_signals
    pub anomaly_signals: Vec<AnomalySignal>,
    /// CDC buffer trends from df_cdc_buffer_trends
    pub cdc_trends: Vec<CdcBufferTrend>,
    /// Scheduling interference from df_scheduling_interference
    pub interference: Vec<InterferenceRecord>,
    /// Rolling efficiency from df_efficiency_rolling
    pub efficiency_rolling: Vec<EfficiencyRollingRecord>,
    /// Threshold advice from df_threshold_advice
    pub threshold_advice: Vec<ThresholdAdviceRecord>,
    /// Retention warning from dog_feeding_status()
    pub retention_warning: Option<String>,
}

#[derive(Default)]
pub struct CacheStore {
    pub delta_sql: HashMap<String, String>,
    pub ddl: HashMap<String, String>,
    pub diagnosed_errors: HashMap<String, Vec<DiagnosedError>>,
    pub explain_mode: HashMap<String, ExplainRefreshMode>,
    pub source_detail: HashMap<String, Vec<SourceTableInfo>>,
    pub refresh_history: HashMap<String, Vec<RefreshHistoryEntry>>,
    pub auxiliary_columns: HashMap<String, Vec<AuxiliaryColumn>>,
    pub change_activity: HashMap<String, ChangeActivity>,
}

#[derive(Default)]
pub struct ConnectionState {
    pub last_poll: Option<DateTime<Utc>>,
    pub connected: bool,
    pub reconnecting: bool,
    pub poll_interval_ms: u64,
    pub error_message: Option<String>,
    pub poll_failure_count: usize,
    pub poll_total_count: usize,
}
```

### Migration Strategy

1. Create domain structs in `state.rs` (additive — no breaking changes)
2. Add `impl AppState` accessor methods that delegate to domains:
   ```rust
   impl AppState {
       pub fn stream_tables_ref(&self) -> &[StreamTableInfo] {
           &self.stream_tables.tables
       }
       // ... backward-compat accessors for views not yet migrated
   }
   ```
3. Migrate `poller.rs` to populate domain fields
4. Migrate views one-by-one from `state.field` to `state.domain.field`
5. Remove legacy top-level fields once all views are migrated

### Backward Compatibility

During migration, both `state.stream_tables` (Vec) and
`state.stream_tables.tables` (domain) can coexist. We choose to rename
the domain struct to `StreamTableDomain` to avoid name collision, then
do a single search-and-replace pass per view module.

---

## Architecture: Selective Polling

### Goal

Only run Phase 2 queries that the active view actually uses.

### Design

```rust
// poller.rs

/// Declares which data categories the active view needs.
#[derive(Default, Clone)]
pub struct DataSubscriptions {
    // Phase 1 (always fetched)
    // stream_tables, health_checks, cdc_buffers, dag_edges, guc_params,
    // diagnostics, efficiency, refresh_log, workers, fuses, watermarks, triggers

    // Phase 2 (selectively fetched)
    pub needs_dedup_stats: bool,
    pub needs_cdc_health: bool,
    pub needs_quick_health: bool,
    pub needs_source_gates: bool,
    pub needs_watermark_alignment: bool,
    pub needs_shared_buffer_stats: bool,
    pub needs_diamond_scc: bool,
    pub needs_scheduler_overhead: bool,

    // Dog-feeding (only when enabled, Phase 2)
    pub needs_dog_feeding_status: bool,
    pub needs_anomaly_signals: bool,
    pub needs_cdc_trends: bool,
    pub needs_interference: bool,
    pub needs_efficiency_rolling: bool,
    pub needs_threshold_advice: bool,
}

impl DataSubscriptions {
    /// Compute subscriptions from the active view.
    pub fn for_view(view: View, dog_feeding_enabled: bool) -> Self {
        let mut subs = Self::default();

        // Dog-feeding status is always checked (lightweight)
        subs.needs_dog_feeding_status = true;
        // Quick health is always needed for the header bar
        subs.needs_quick_health = true;

        match view {
            View::Dashboard => {
                subs.needs_anomaly_signals = dog_feeding_enabled;
            }
            View::Detail => {
                subs.needs_anomaly_signals = dog_feeding_enabled;
            }
            View::Cdc => {
                subs.needs_cdc_health = true;
                subs.needs_dedup_stats = true;
                subs.needs_shared_buffer_stats = true;
                subs.needs_cdc_trends = dog_feeding_enabled;
            }
            View::Workers => {
                subs.needs_scheduler_overhead = true;
                subs.needs_interference = dog_feeding_enabled;
            }
            View::Diagnostics => {
                subs.needs_scheduler_overhead = true;
                subs.needs_efficiency_rolling = dog_feeding_enabled;
                subs.needs_threshold_advice = dog_feeding_enabled;
            }
            View::Graph => {
                subs.needs_diamond_scc = true;
            }
            View::Watermarks => {
                subs.needs_source_gates = true;
                subs.needs_watermark_alignment = true;
            }
            View::Issues => {
                subs.needs_anomaly_signals = dog_feeding_enabled;
            }
            // RefreshLog, Config, Health, Alerts, Fuse, DeltaInspector:
            // no Phase 2 subscriptions needed
            _ => {}
        }

        subs
    }
}
```

### Poller Integration

```rust
pub async fn poll_all(
    client: &Client,
    state: &mut AppState,
    subs: &DataSubscriptions,
) {
    // Phase 1: always (existing tokio::join!)
    let (r_st, r_health, r_cdc, r_dag, r_diag, r_eff, r_gucs, r_log, r_workers, r_fuses, r_wm, r_triggers) = tokio::join!(
        poll_stream_tables_query(client),
        poll_health_query(client),
        poll_cdc_query(client),
        poll_dag_query(client),
        poll_diagnostics_query(client),
        poll_efficiency_query(client),
        poll_gucs_query(client),
        poll_refresh_log_query(client),
        poll_workers_query(client),
        poll_fuses_query(client),
        poll_watermarks_query(client),
        poll_triggers_query(client),
    );
    // ... apply results to state ...

    // Phase 2: selective
    if subs.needs_dedup_stats {
        if let Ok(ds) = poll_dedup_stats_query(client).await {
            state.diagnostics.dedup_stats = ds;
        }
    }
    if subs.needs_cdc_health {
        if let Ok(ch) = poll_cdc_health_query(client).await {
            state.cdc.health = ch;
        }
    }
    // ... etc for each subscription ...

    // Dog-feeding subscriptions (only when status check says enabled)
    if subs.needs_dog_feeding_status {
        if let Ok(status) = poll_dog_feeding_status_query(client).await {
            state.dog_feeding.enabled = status.is_some();
            state.dog_feeding.status = status;
            state.dog_feeding.retention_warning =
                state.dog_feeding.status.as_ref()
                    .and_then(|s| s.retention_warning.clone());
        }
    }
    if state.dog_feeding.enabled {
        if subs.needs_anomaly_signals {
            if let Ok(signals) = poll_anomaly_signals_query(client).await {
                state.dog_feeding.anomaly_signals = signals;
            }
        }
        if subs.needs_cdc_trends {
            if let Ok(trends) = poll_cdc_trends_query(client).await {
                state.dog_feeding.cdc_trends = trends;
            }
        }
        if subs.needs_interference {
            if let Ok(records) = poll_interference_query(client).await {
                state.dog_feeding.interference = records;
            }
        }
        if subs.needs_efficiency_rolling {
            if let Ok(records) = poll_efficiency_rolling_query(client).await {
                state.dog_feeding.efficiency_rolling = records;
            }
        }
        if subs.needs_threshold_advice {
            if let Ok(records) = poll_threshold_advice_query(client).await {
                state.dog_feeding.threshold_advice = records;
            }
        }
    }
}
```

### Performance Impact

Assuming 10 ms per Phase 2 query and 9 Phase 2 queries:
- **Before:** 9 × 10 ms = 90 ms wasted per poll on non-subscribed queries
- **After:** Only subscribed queries run; typical Dashboard poll saves ~50 ms
- Dog-feeding queries add ~30–50 ms when enabled and subscribed

---

## Architecture: Poller Extraction

### Goal

Separate query functions from state mutation so both can be unit-tested
independently.

### Design

Create `poller/` as a module directory:

```
src/poller/
├── mod.rs       # poll_all() orchestration + action execution
├── fetchers.rs  # Pure query functions: fetch_*() → Result<T, PgErr>
└── updaters.rs  # Pure state mutations: apply_*(&mut AppState, T)
```

**Fetchers** (testable with mock client):
```rust
// poller/fetchers.rs
pub async fn fetch_stream_tables(client: &Client) -> Result<Vec<StreamTableInfo>, PgErr> { ... }
pub async fn fetch_dog_feeding_status(client: &Client) -> Result<Option<DogFeedingStatus>, PgErr> { ... }
pub async fn fetch_anomaly_signals(client: &Client) -> Result<Vec<AnomalySignal>, PgErr> { ... }
// ... one per poll query
```

**Updaters** (testable without async or DB):
```rust
// poller/updaters.rs
pub fn apply_stream_tables(state: &mut AppState, tables: Vec<StreamTableInfo>) {
    // preserve sparkline_data from previous poll
    state.stream_tables.tables = tables;
}
pub fn apply_dog_feeding_status(state: &mut AppState, status: Option<DogFeedingStatus>) {
    state.dog_feeding.enabled = status.is_some();
    state.dog_feeding.retention_warning = status.as_ref()
        .and_then(|s| s.retention_warning.clone());
    state.dog_feeding.status = status;
}
pub fn apply_anomaly_signals(state: &mut AppState, signals: Vec<AnomalySignal>) {
    state.dog_feeding.anomaly_signals = signals;
}
// ... one per domain
```

---

## Architecture: CLI/TUI Command Unification

### Goal

A single canonical implementation for each command, shared by CLI subcommands
and TUI palette commands.

### Design

Create `commands/domain.rs` with shared command logic:

```rust
// commands/domain.rs

pub async fn enable_dog_feeding(client: &Client) -> Result<String, CliError> {
    client.execute("SELECT pgtrickle.setup_dog_feeding()", &[]).await?;
    Ok("Dog-feeding enabled".to_string())
}

pub async fn disable_dog_feeding(client: &Client) -> Result<String, CliError> {
    client.execute("SELECT pgtrickle.teardown_dog_feeding()", &[]).await?;
    Ok("Dog-feeding disabled".to_string())
}

pub async fn dog_feeding_status(
    client: &Client,
    format: OutputFormat,
) -> Result<String, CliError> {
    let rows = client.query("SELECT * FROM pgtrickle.dog_feeding_status()", &[]).await?;
    format_rows(rows, format)
}

pub async fn export_dag(
    client: &Client,
    format: &str,  // "mermaid" | "dot"
) -> Result<String, CliError> {
    let rows = client.query(
        "SELECT pgtrickle.explain_dag($1)",
        &[&format],
    ).await?;
    Ok(rows.first().and_then(|r| r.get::<_, Option<String>>(0)).unwrap_or_default())
}
```

**CLI uses it:**
```rust
// commands/dog_feeding.rs
pub async fn execute(client: &Client, args: &DogFeedingArgs) -> Result<(), CliError> {
    match args.action {
        DogFeedingAction::Enable => println!("{}", domain::enable_dog_feeding(client).await?),
        DogFeedingAction::Disable => println!("{}", domain::disable_dog_feeding(client).await?),
        DogFeedingAction::Status => println!("{}", domain::dog_feeding_status(client, args.format).await?),
    }
    Ok(())
}
```

**TUI uses it:**
```rust
// poller.rs → execute_action()
ActionRequest::DogFeedingEnable => {
    match domain::enable_dog_feeding(client).await {
        Ok(msg) => PollMsg::ActionResult(ActionResult { success: true, message: msg }),
        Err(e) => PollMsg::ActionResult(ActionResult { success: false, message: e.to_string() }),
    }
}
```

---

## Dog-Feeding Data Model

### New State Types

```rust
// state.rs

#[derive(Clone, Debug, Default, Serialize)]
pub struct DogFeedingStatus {
    pub active_count: i32,
    pub total_count: i32,
    pub setup_at: Option<String>,
    pub retention_warning: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AnomalySignal {
    pub st_name: String,
    pub anomaly_type: String,      // DURATION_SPIKE, ERROR_BURST, MODE_OSCILLATION
    pub severity: String,           // CRITICAL, WARNING
    pub detected_at: String,
    pub delta_from_baseline: f64,
    pub window_seconds: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CdcBufferTrend {
    pub source_table: String,
    pub buffer_counts: Vec<f64>,    // last N cycle counts for sparkline
    pub growth_rate: f64,
    pub spill_risk: Option<String>, // IMMINENT, ELEVATED, or None
}

#[derive(Clone, Debug, Serialize)]
pub struct InterferenceRecord {
    pub st_a: String,
    pub st_b: String,
    pub overlap_count: i64,
    pub total_overlap_ms: f64,
    pub last_seen: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EfficiencyRollingRecord {
    pub st_name: String,
    pub diff_count: i64,
    pub full_count: i64,
    pub avg_diff_ms: f64,
    pub avg_full_ms: f64,
    pub diff_speedup: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThresholdAdviceRecord {
    pub st_name: String,
    pub recommended_threshold: f64,
    pub confidence: f64,
    pub sla_headroom_pct: f64,
    pub sla_breach_risk: bool,      // NEW: DF-21
}
```

### Dog-Feeding SQL Queries

| Query | Source | Fields | Used By |
|-------|--------|--------|---------|
| `SELECT * FROM pgtrickle.dog_feeding_status()` | SQL func | `active_count`, `total_count`, `setup_at`, `retention_warning` | TUI-8, TUI-12 |
| `SELECT * FROM pgtrickle.df_anomaly_signals` | DF-2 ST | `st_name`, `anomaly_type`, `severity`, `detected_at`, `delta_from_baseline`, `window_seconds` | TUI-1, TUI-2, TUI-10, TUI-13 |
| `SELECT * FROM pgtrickle.df_cdc_buffer_trends` | DF-4 ST | `source_table`, `buffer_counts`, `growth_rate`, `spill_risk` | TUI-3, TUI-4 |
| `SELECT * FROM pgtrickle.df_scheduling_interference` | DF-5 ST | `st_a`, `st_b`, `overlap_count`, `total_overlap_ms`, `last_seen` | TUI-5 |
| `SELECT * FROM pgtrickle.df_efficiency_rolling` | DF-1 ST | `st_name`, `diff_count`, `full_count`, `avg_diff_ms`, `avg_full_ms`, `diff_speedup` | TUI-16 |
| `SELECT * FROM pgtrickle.df_threshold_advice` | DF-3 ST | `st_name`, `recommended_threshold`, `confidence`, `sla_headroom_pct`, `sla_breach_risk` | Diagnostics |

---

## Feature Specifications

### TUI-1 — Anomaly Detection View

**Key:** `a`
**Data source:** `df_anomaly_signals` via `state.dog_feeding.anomaly_signals`

New view accessible via `a` key. Renders a table of active anomaly signals
from the `df_anomaly_signals` dog-feeding stream table.

**Columns:**
| Column | Width | Source |
|--------|-------|--------|
| Stream Table | 25 | `st_name` |
| Type | 18 | `anomaly_type` (friendly labels: "Duration Spike", "Error Burst", "Mode Oscillation") |
| Severity | 10 | Color-coded: CRITICAL in red, WARNING in yellow |
| Detected | 19 | `detected_at` timestamp |
| Delta | 10 | `delta_from_baseline` (e.g., "+3.2×") |
| Window | 8 | `window_seconds` (e.g., "5 min") |

**Empty state:** When dog-feeding is not enabled:
```
┌─ Anomaly Detection ──────────────────────────────┐
│                                                   │
│  Dog-feeding is not active.                       │
│  Run :dog-feeding enable for anomaly detection.   │
│                                                   │
└───────────────────────────────────────────────────┘
```

**Sorting:** Default sort by severity DESC, then detected_at DESC.
**Filtering:** Cross-view `/` filter applies to `st_name` and `anomaly_type`.

### TUI-2 — Dashboard Anomaly Badge

**View:** Dashboard (key `1`)
**Data source:** `state.dog_feeding.anomaly_signals.len()`

In `render_status_ribbon()`, after existing status counters (active, paused,
error, suspended), append an anomaly badge:

```
 ● 5 active  ⏸ 1 paused  ✗ 0 errors  🔍 3 anomalies
```

- Badge text: `🔍 N anomalies` (cyan icon, red count when N > 0)
- Hidden when N = 0 or dog-feeding is off
- Implementation: ~10 LOC in `dashboard.rs::render_status_ribbon()`

### TUI-3 — CDC Health Sparkline Column

**View:** CDC Health (key `6`)
**Data source:** `state.dog_feeding.cdc_trends`

When dog-feeding is active, each source table row in the CDC buffers section
gains a 20-character sparkline column showing buffer row-count trend over
the last 10 poll cycles.

**Rendering:**
- Use braille block characters (`⣀⣤⣶⣿` etc.) for compact sparkline
- Data: `cdc_trends.buffer_counts` (Vec<f64>, last 10 values)
- Falls back to `—` when dog-feeding is off or no trend data available
- Column header: `Trend`

### TUI-4 — CDC Health Spill-Risk Badge

**View:** CDC Health (key `6`)
**Data source:** `state.dog_feeding.cdc_trends.spill_risk` + `state.cdc.health`

When `check_cdc_health()` reports spill risk via OPS-2 enrichment:
- `IMMINENT` → red badge, overrides normal buffer-size highlight
- `ELEVATED` → orange badge
- No risk → no badge

Implementation: extend the existing CDC view's row rendering to check
`cdc_trends` for matching `source_table` and apply badge.

### TUI-5 — Workers Interference Sub-Tab

**View:** Workers (key `w`)
**Data source:** `state.dog_feeding.interference`

New `Interference` tab (second tab, switched with `Tab` key) in the Workers
view. Shows rows from `df_scheduling_interference`:

**Columns:**
| Column | Width | Source |
|--------|-------|--------|
| Table A | 25 | `st_a` |
| Table B | 25 | `st_b` |
| Overlaps | 10 | `overlap_count` |
| Total Duration | 14 | `total_overlap_ms` (formatted as "1.2s") |
| Last Seen | 19 | `last_seen` |

**Sorting:** Default by `overlap_count` DESC.
**Empty state:** Same pattern as TUI-1 (dog-feeding hint).

App struct gains `workers_tab: usize` (0=Overview, 1=Interference).
`Tab` key cycles between tabs when in Workers view.

### TUI-6 — Workers Scheduler Overhead Bar

**View:** Workers (key `w`)
**Data source:** `state.diagnostics.scheduler_overhead`

Below the worker pool table (both tabs), render a one-line summary:

```
scheduler: busy 12.3% | queue 3 | latency 45ms | df_fraction 8.2%
```

- Source: existing `scheduler_overhead` from `pgtrickle.scheduler_overhead()`
- Greyed out when `scheduler_overhead` is `None` (< 5 refresh cycles)
- Note: Diagnostics view already renders this (UX-7); reuse the rendering
  logic as a shared helper

### TUI-7 — Dependencies Mermaid/DOT Export

**View:** Dependencies (key `3`)
**Data source:** `pgtrickle.explain_dag()` (on-demand fetch)

Press `x` to call `pgtrickle.explain_dag('mermaid')` and display the result
in a scrollable overlay (same pattern as DDL overlay in Detail view).

**Node coloring in display:**
- Dog-feeding nodes (`df_*`): green
- User nodes: blue (default)
- Suspended nodes: red
- Error nodes: red, bold

Press `Ctrl+E` in the overlay to export to file
(`/tmp/pgtrickle_dag_<timestamp>.md`).

Implementation: new `ActionRequest::FetchExplainDag(String)` for format
selection. New overlay state `mermaid_overlay: Option<String>` in `App`.

### TUI-8 — Header Dog-Feeding Status Badge

**View:** All views (header bar)
**Data source:** `state.dog_feeding.status`

In the shared header bar (rendered by `draw_header()` in `app.rs`), add:

- `df:5/5` in green when all 5 dog-feeding STs are active
- `df:3/5` in yellow when some are active
- `df:off` dimmed when dog-feeding not configured
- `df:⚠` in orange when `retention_warning` is set

Position: after the scheduler indicator, before the poll interval.

### TUI-9 — Dog-Feeding Commands in Palette

**View:** Command palette (`:` mode)

Add three commands:
- `dog-feeding enable` → calls `setup_dog_feeding()` → toast "Dog-feeding enabled (5 stream tables created)"
- `dog-feeding disable` → confirmation dialog → calls `teardown_dog_feeding()` → toast "Dog-feeding disabled"
- `dog-feeding status` → calls `dog_feeding_status()` → toast with summary

Implementation:
1. Add `DogFeedingEnable`, `DogFeedingDisable` variants to `ActionRequest`
2. Add `("dog-feeding enable", ...)`, `("dog-feeding disable", ...)`,
   `("dog-feeding status", ...)` to command list in `CommandPalette`
3. Add execution handlers in `execute_action()` using unified domain logic
4. `dog-feeding disable` requires confirmation (same as `pause`)

### TUI-10 — Detail View Anomaly Summary

**View:** Detail (key `2`)
**Data source:** `state.dog_feeding.anomaly_signals` filtered by selected ST

In the Properties section of the Detail view, after the existing rows
(status, mode, staleness, etc.), add an `Anomalies` row:

- When anomalies exist: `⚠ 2 active: DURATION_SPIKE, ERROR_BURST` (red)
- When no anomalies: `—` (dim)
- When dog-feeding off: row not shown

### TUI-11 — Refresh Log Auto Tag

**View:** Refresh Log (key `4`)
**Data source:** `initiated_by` column in `pgt_refresh_history`

When a refresh log entry has `initiated_by = 'DOG_FEED'`, display an
orange `[auto]` tag in the Mode column, after the mode label:

```
 differential [auto]   3.2ms   +42 -3
```

Implementation:
1. Extend `poll_refresh_log_query()` to include `initiated_by` column
2. Add `initiated_by: Option<String>` to `RefreshLogEntry` state struct
3. In `refresh_log.rs`, check for `DOG_FEED` value and render tag

### TUI-12 — First-Launch Dog-Feeding Toast

**View:** Any (on first poll result)
**Data source:** `state.dog_feeding.enabled`

On the first successful poll, if `dog_feeding_status()` returns zero active
stream tables (dog-feeding not set up), show a one-time info toast:

```
Dog-feeding not active. Run :dog-feeding enable for enhanced monitoring.
```

- Dismissed automatically after 10 s or on any keypress
- Only shown once per TUI session (tracked by `shown_df_hint: bool` in `App`)
- Not shown if dog-feeding is already active

### TUI-13 — Anomaly Signals as Issues

**View:** Issues (key `i`)
**Data source:** `state.dog_feeding.anomaly_signals`

Extend `detect_issues()` in `state.rs` to include anomaly signals when
dog-feeding is active:

```rust
// In detect_issues():
if self.dog_feeding.enabled {
    for signal in &self.dog_feeding.anomaly_signals {
        let severity = match signal.severity.as_str() {
            "CRITICAL" => IssueSeverity::Critical,
            _ => IssueSeverity::Warning,
        };
        issues.push(Issue {
            severity,
            category: "Anomaly".to_string(),
            table: signal.st_name.clone(),
            message: format!(
                "{}: {} (delta: {:.1}×)",
                signal.anomaly_type,
                signal.severity,
                signal.delta_from_baseline
            ),
            blast_radius: 1,
        });
    }
}
```

### TUI-14 — Dog-Feed Alert Styling

**View:** Alerts (key `9`)
**Data source:** `state.monitoring.alerts` (filtered by event type)

Events with `event_type = "dog_feed_anomaly"` get a distinct cyan `🔍` icon
instead of the standard `⚠` or `✗`. Applied in `alert.rs` rendering.

### TUI-15 — Dashboard Snapshot Tests

**View:** Dashboard (key `1`)
**Tests:** `views/mod.rs::snapshot_tests`

Add snapshot tests for Dashboard view covering all rendering branches:
- `test_dashboard_standard_80x24` — standard layout
- `test_dashboard_wide_160x40` — wide layout with sparklines and DAG minimap
- `test_dashboard_empty_80x24` — empty state (no stream tables)
- `test_dashboard_with_anomalies_80x24` — badge visible (uses `sample_state_dog_feeding()`)
- `test_dashboard_narrow_60x20` — narrow terminal, graceful truncation

### TUI-16 — Diagnostics Efficiency Summary

**View:** Diagnostics (key `5`)
**Data source:** `state.dog_feeding.efficiency_rolling`

When dog-feeding is active, add a compact "Efficiency" panel below the
existing diagnostics recommendations:

```
┌─ Dog-Feeding Efficiency ─────────────────────────┐
│ Avg Diff Speedup: 4.2×  |  Diff: 847  Full: 23  │
│ Last Hour: 98% differential                       │
└───────────────────────────────────────────────────┘
```

- Computed from `efficiency_rolling`: aggregate `diff_speedup` across all STs,
  sum `diff_count` and `full_count`
- Shows `—` when dog-feeding is off

---

## Backend Enhancements

### DF-21 — sla_breach_risk Column

Add a `sla_breach_risk` boolean derived column to the `df_threshold_advice`
stream table query:

```sql
sla_breach_risk = (avg_diff_ms > freshness_deadline_ms)
```

This completes the v0.20.0 exit criterion UX-8 that was left unchecked.

**Files:** `src/api/dog_feeding.rs` (DF-3 query definition)
**Test:** Synthetic-data E2E test asserting `sla_breach_risk = true` when
  `avg_diff_ms` exceeds `freshness_deadline_ms`

### DF-22 — auto_apply Full Mode

Implement the `dog_feeding_auto_apply = 'full'` scheduling mode documented
as "future enhancement" in `docs/CONFIGURATION.md`.

When `full` is active, `dog_feeding_auto_apply_tick()` additionally:
1. Reads `df_scheduling_interference.overlap_count` for all pairs
2. Widens the dispatch interval for high-overlap pairs (> 5 overlaps)
   by 20% per 5 overlaps, capped at 2× baseline
3. Applies the existing threshold recommendations from `df_threshold_advice`

**Files:** `src/scheduler.rs` (tick function), `src/config.rs` (mode docs)
**Test:** E2E test verifying dispatch interval widened after inserting
  synthetic interference data

### DF-23 — Retention Warning

Add a `retention_warning` column to `dog_feeding_status()` output that is
set when `pg_trickle.history_retention_days` is below the minimum window
needed by DF-1 (`df_efficiency_rolling`) and DF-3 (`df_threshold_advice`).

Minimum requirement: retention ≥ 1 day (both STs use rolling 24h windows).

**Files:** `src/api/dog_feeding.rs` (`dog_feeding_status()` function)
**Test:** E2E test setting `history_retention_days = 0` and verifying
  warning is non-NULL

### DF-24 — recommend_refresh_mode Reads DF

When dog-feeding is active, `recommend_refresh_mode()` reads from the
maintained `df_threshold_advice` stream table instead of recomputing on
demand. Same results, lower latency. Falls back to on-demand computation
when dog-feeding is off.

**Files:** `src/api/dog_feeding.rs`, `src/refresh.rs`
  (recommend_refresh_mode integration)
**Test:** E2E test verifying that with dog-feeding active,
  `recommend_refresh_mode()` returns data consistent with
  `df_threshold_advice`

---

## CLI Integration

### CLI-1 — Dog-Feeding Subcommand Group

Add `pgtrickle dog-feeding` with three subcommands:

```
pgtrickle dog-feeding enable          # calls setup_dog_feeding()
pgtrickle dog-feeding disable         # calls teardown_dog_feeding()
pgtrickle dog-feeding status          # calls dog_feeding_status()
pgtrickle dog-feeding status --format json   # JSON output
```

**Files:**
- `pgtrickle-tui/src/cli.rs` — add `DogFeeding` variant to `Commands` enum
- `pgtrickle-tui/src/commands/dog_feeding.rs` — new command module
- `pgtrickle-tui/src/commands/domain.rs` — shared logic (see architecture section)
- `pgtrickle-tui/src/main.rs` — dispatch

### CLI-2 — Graph Format Flag

Add `--format ascii|mermaid|dot` to the existing `pgtrickle graph` command:

```
pgtrickle graph                       # existing ASCII output
pgtrickle graph --format mermaid      # calls explain_dag('mermaid')
pgtrickle graph --format dot          # calls explain_dag('dot')
```

**Files:**
- `pgtrickle-tui/src/cli.rs` — add `format` field to `GraphArgs`
- `pgtrickle-tui/src/commands/graph.rs` — dispatch based on format
- `pgtrickle-tui/src/commands/domain.rs` — `export_dag()` shared logic

---

## Test Strategy

### New State Structs Requiring Fixture Builders

| Struct | Fixture Builder | Used By |
|--------|-----------------|---------|
| `DogFeedingStatus` | `dog_feeding_status(active, total, setup_at, warning)` | TUI-8, TUI-12 |
| `AnomalySignal` | `anomaly_signal(st, type_, severity, delta)` | TUI-1, TUI-2, TUI-10, TUI-13 |
| `CdcBufferTrend` | `cdc_buffer_trend(source, counts, growth, risk)` | TUI-3, TUI-4 |
| `InterferenceRecord` | `interference_record(st_a, st_b, overlaps, duration)` | TUI-5 |
| `EfficiencyRollingRecord` | `efficiency_rolling(st, diff_count, full_count, speedup)` | TUI-16 |
| `ThresholdAdviceRecord` | `threshold_advice(st, threshold, confidence, headroom, breach)` | Diagnostics |

### Extended Fixtures

Add `sample_state_dog_feeding()` to `test_fixtures.rs`:

```rust
pub fn sample_state_dog_feeding() -> AppState {
    let mut state = sample_state_full();  // from Part 2
    state.dog_feeding.enabled = true;
    state.dog_feeding.status = Some(dog_feeding_status(5, 5, Some("2026-04-15T10:00:00Z"), None));
    state.dog_feeding.anomaly_signals = vec![
        anomaly_signal("order_totals", "DURATION_SPIKE", "WARNING", 2.3),
        anomaly_signal("revenue_daily", "ERROR_BURST", "CRITICAL", 5.1),
    ];
    state.dog_feeding.cdc_trends = vec![
        cdc_buffer_trend("orders", vec![10.0, 12.0, 15.0, 18.0, 22.0], 1.5, None),
        cdc_buffer_trend("events", vec![5.0, 5.0, 8.0, 12.0, 50.0], 4.2, Some("ELEVATED")),
    ];
    state.dog_feeding.interference = vec![
        interference_record("order_totals", "revenue_daily", 12, 3400.0),
    ];
    state.dog_feeding.efficiency_rolling = vec![
        efficiency_rolling("order_totals", 847, 23, 4.2),
        efficiency_rolling("revenue_daily", 612, 8, 6.1),
    ];
    state
}
```

### SQL Stub Extensions for Contract Tests

New stubs required in `test_db.rs`:

| SQL Function/View | Returns | Stub Data |
|---|---|---|
| `dog_feeding_status()` | `active_count int, total_count int, setup_at text, retention_warning text` | `5, 5, '2026-04-15', NULL` |
| `df_anomaly_signals` (view) | `st_name text, anomaly_type text, severity text, detected_at text, delta_from_baseline float8, window_seconds int8` | Two rows |
| `df_cdc_buffer_trends` (view) | `source_table text, buffer_counts float8[], growth_rate float8, spill_risk text` | Two rows |
| `df_scheduling_interference` (view) | `st_a text, st_b text, overlap_count int8, total_overlap_ms float8, last_seen text` | One row |
| `df_efficiency_rolling` (view) | `st_name text, diff_count int8, full_count int8, avg_diff_ms float8, avg_full_ms float8, diff_speedup float8` | Two rows |
| `df_threshold_advice` (view) | `st_name text, recommended_threshold float8, confidence float8, sla_headroom_pct float8, sla_breach_risk bool` | Two rows |
| `setup_dog_feeding()` | `void` | No-op |
| `teardown_dog_feeding()` | `void` | No-op |
| `explain_dag(text)` | `text` | Mermaid string |

### Snapshot Test Plan

| Feature | Test Name | State | Terminal Size | Key Assertions |
|---------|-----------|-------|---------------|----------------|
| TUI-1 | `test_anomaly_view_populated_80x24` | `sample_state_dog_feeding()` | 80×24 | Signal table with severity colors, type labels |
| TUI-1 | `test_anomaly_view_empty_80x24` | `sample_state()` (df off) | 80×24 | Empty state with setup hint |
| TUI-2 | `test_dashboard_anomaly_badge_80x24` | `sample_state_dog_feeding()` | 80×24 | Ribbon shows `🔍 2 anomalies` |
| TUI-3 | `test_cdc_sparkline_with_trends_100x30` | `sample_state_dog_feeding()` | 100×30 | Sparkline chars visible in Trend column |
| TUI-3 | `test_cdc_sparkline_no_df_100x30` | `sample_state()` | 100×30 | `—` in Trend column |
| TUI-4 | `test_cdc_spill_risk_badge_100x30` | Trend with `spill_risk = "ELEVATED"` | 100×30 | Orange ELEVATED badge visible |
| TUI-5 | `test_workers_interference_tab_100x30` | `sample_state_dog_feeding()` | 100×30 | Interference table with overlap data |
| TUI-5 | `test_workers_interference_empty_100x30` | `sample_state()` | 100×30 | Empty state with hint |
| TUI-6 | `test_workers_overhead_bar_100x30` | Scheduler overhead populated | 100×30 | One-line summary visible |
| TUI-7 | `test_graph_mermaid_overlay_80x30` | Mermaid text in overlay state | 80×30 | Mermaid markdown visible, scrollable |
| TUI-8 | `test_header_df_active_80x24` | `sample_state_dog_feeding()` | 80×24 | `df:5/5` in green |
| TUI-8 | `test_header_df_off_80x24` | `sample_state()` | 80×24 | `df:off` dimmed |
| TUI-8 | `test_header_df_warning_80x24` | Status with `retention_warning` | 80×24 | `df:⚠` in orange |
| TUI-10 | `test_detail_anomaly_row_80x40` | Selected ST with anomalies | 80×40 | `⚠ 1 active: DURATION_SPIKE` |
| TUI-11 | `test_refresh_log_auto_tag_80x24` | Log entry with `initiated_by = DOG_FEED` | 80×24 | Orange `[auto]` tag |
| TUI-13 | `test_issues_with_anomalies_80x24` | `sample_state_dog_feeding()` | 80×24 | Anomaly category in issues list |
| TUI-14 | `test_alert_dog_feed_icon_80x24` | Alert with `dog_feed_anomaly` event | 80×24 | Cyan `🔍` icon |
| TUI-15 | `test_dashboard_standard_80x24` | `sample_state()` | 80×24 | Standard layout, no panics |
| TUI-15 | `test_dashboard_wide_160x40` | `sample_state()` | 160×40 | Wide layout with sparklines + minimap |
| TUI-15 | `test_dashboard_empty_80x24` | `empty_state()` | 80×24 | Empty placeholder |
| TUI-16 | `test_diagnostics_efficiency_panel_80x40` | `sample_state_dog_feeding()` | 80×40 | Efficiency panel with speedup and counts |
| TUI-16 | `test_diagnostics_no_df_80x40` | `sample_state()` | 80×40 | No efficiency panel |

### Unit Test Plan

| Module | Test Name | What It Verifies |
|--------|-----------|------------------|
| `state.rs` | `test_detect_issues_includes_anomalies` | `detect_issues()` adds WARNING/CRITICAL for anomaly signals |
| `state.rs` | `test_detect_issues_no_anomalies_when_df_off` | `detect_issues()` skips anomalies when `dog_feeding.enabled = false` |
| `state.rs` | `test_dog_feeding_domain_default` | `DogFeedingDomain::default()` has `enabled = false`, empty vecs |
| `state.rs` | `test_anomaly_signal_severity_mapping` | CRITICAL maps to `IssueSeverity::Critical`, WARNING to Warning |
| `app.rs` | `test_key_a_switches_to_anomaly_view` | `a` key dispatches to `View::Anomalies` |
| `app.rs` | `test_tab_in_workers_cycles_tabs` | Tab cycles between Overview and Interference |
| `app.rs` | `test_x_in_graph_fetches_mermaid` | `x` key dispatches `FetchExplainDag` action |
| `app.rs` | `test_dog_feeding_enable_command` | `:dog-feeding enable` parsed correctly |
| `app.rs` | `test_dog_feeding_disable_requires_confirm` | `:dog-feeding disable` shows confirmation dialog |
| `cli.rs` | `test_dog_feeding_enable_subcommand` | `pgtrickle dog-feeding enable` parses correctly |
| `cli.rs` | `test_graph_format_mermaid` | `pgtrickle graph --format mermaid` parses correctly |
| `cli.rs` | `test_graph_format_default_ascii` | `pgtrickle graph` defaults to ASCII |
| `poller.rs` | `test_subscriptions_for_dashboard` | Dashboard subscribes to anomaly_signals but not interference |
| `poller.rs` | `test_subscriptions_for_cdc` | CDC subscribes to cdc_health, dedup, shared_buffer, cdc_trends |
| `poller.rs` | `test_subscriptions_for_workers` | Workers subscribes to scheduler_overhead, interference |
| `poller.rs` | `test_subscriptions_df_disabled` | All DF subscriptions false when dog_feeding_enabled = false |

### Degradation Test Plan

| Test | What It Verifies |
|------|------------------|
| `test_poll_dog_feeding_status_missing` | TUI renders with `df:off` when `dog_feeding_status()` is absent |
| `test_poll_anomaly_signals_missing` | Anomaly view shows empty state when `df_anomaly_signals` query fails |
| `test_poll_cdc_trends_missing` | CDC sparkline shows `—` when `df_cdc_buffer_trends` query fails |
| `test_poll_interference_missing` | Workers Interference tab shows empty when query fails |
| `test_poll_efficiency_rolling_missing` | Diagnostics hides efficiency panel when query fails |
| `test_anomaly_view_v020_compat` | TUI works when connected to v0.20 (no new columns like retention_warning) |

---

## Implementation Phases

### Phase T15 — Architecture Foundation (Day 1–3)

**Goal:** Decompose AppState into domain structs. Extract poller logic.
Add selective polling infrastructure. No user-visible changes.

| Item | Description | Effort |
|------|-------------|--------|
| T15a | Define 8 domain structs in `state.rs`: `StreamTableDomain`, `CdcDomain`, `DiagnosticsDomain`, `MonitoringDomain`, `SchedulingDomain`, `WatermarkDomain`, `ConfigDomain`, `DogFeedingDomain` | 2h |
| T15b | Define `CacheStore` and `ConnectionState` structs | 1h |
| T15c | Refactor `AppState` to use domain structs; add backward-compat accessor methods | 3h |
| T15d | Migrate all views from `state.field` to `state.domain.field` (14 view files) | 4h |
| T15e | Migrate `detect_issues()` and `compute_cascade_staleness()` to use domain paths | 1h |
| T15f | Create `poller/` directory: extract `poller/fetchers.rs` (21 `fetch_*()` functions) | 3h |
| T15g | Create `poller/updaters.rs` (21 `apply_*()` functions) | 2h |
| T15h | Refactor `poller/mod.rs` to orchestrate fetchers + updaters | 2h |
| T15i | Define `DataSubscriptions` struct and `DataSubscriptions::for_view()` | 1h |
| T15j | Integrate subscriptions into `poll_all()`: Phase 2 queries check subscription flags | 2h |
| T15k | Pass active `View` from app loop → poller for subscription computation | 1h |
| T15l | Create `commands/domain.rs` skeleton with shared logic for existing commands (refresh, pause, resume, fuse reset, repair, gate/ungate) | 2h |
| T15m | **Tests:** Verify all existing snapshot tests still pass after refactoring | 1h |
| T15n | **Tests:** Add unit tests for `DataSubscriptions::for_view()` (6 views × expected flags) | 1h |
| T15o | **Tests:** Add unit tests for `apply_*()` updater functions (6 critical updaters) | 1h |

**Exit:** `AppState` uses domain structs. Poller respects subscriptions.
All existing tests pass. No user-visible behavior change. `just lint` clean.

### Phase T16 — Dog-Feeding Data Layer (Day 3–5)

**Goal:** Add dog-feeding state types, polling queries, fixture builders,
and contract stubs. No new views yet.

| Item | Description | Effort |
|------|-------------|--------|
| T16a | Add `DogFeedingStatus`, `AnomalySignal`, `CdcBufferTrend`, `InterferenceRecord`, `EfficiencyRollingRecord`, `ThresholdAdviceRecord` to `state.rs` | 2h |
| T16b | Add `DogFeedingDomain` fields to `AppState` (already defined in T15a, now populate types) | 30m |
| T16c | Add `fetch_dog_feeding_status()`, `fetch_anomaly_signals()`, `fetch_cdc_trends()`, `fetch_interference()`, `fetch_efficiency_rolling()`, `fetch_threshold_advice()` to `poller/fetchers.rs` | 3h |
| T16d | Add `apply_dog_feeding_status()`, `apply_anomaly_signals()`, `apply_cdc_trends()`, `apply_interference()`, `apply_efficiency_rolling()` to `poller/updaters.rs` | 1h |
| T16e | Wire dog-feeding fetchers into `poll_all()` under subscription flags | 2h |
| T16f | Add `RefreshLogEntry.initiated_by: Option<String>` field; extend `poll_refresh_log_query()` to fetch `initiated_by` column | 1h |
| T16g | **Fixtures:** Add fixture builders for all 6 new structs in `test_fixtures.rs` | 1h |
| T16h | **Fixtures:** Add `sample_state_dog_feeding()` to `test_fixtures.rs` | 1h |
| T16i | **Contract stubs:** Add 9 new stubs to `test_db.rs` (dog_feeding_status, df_* views, setup/teardown, explain_dag) | 2h |
| T16j | **Contract tests:** 6 new `test_poll_df_*_executes` tests against stubs | 1h |
| T16k | **Degradation tests:** 5 `test_poll_df_*_graceful_missing` tests | 1h |
| T16l | **Unit tests:** Dog-feeding domain defaults, status parsing, anomaly signal severity | 1h |

**Exit:** Dog-feeding data flows from DB → state. All new types have fixtures.
Contract stubs match expected column signatures. Degradation verified.

### Phase T17 — Dog-Feeding TUI Views (Day 5–9)

**Goal:** Implement all 16 TUI items. New Anomaly view, enhanced existing
views, header badge, command palette commands, first-launch toast.

| Item | Description | Effort |
|------|-------------|--------|
| T17a | **TUI-1:** New `views/anomaly.rs` — Anomaly Detection view with table, severity colors, empty state hint | 3h |
| T17b | **TUI-1:** Add `View::Anomalies` variant, `a` key binding, `list_len()` case | 1h |
| T17c | **TUI-2:** Dashboard anomaly badge in `render_status_ribbon()` | 1h |
| T17d | **TUI-3:** CDC Health sparkline column — braille rendering from `cdc_trends.buffer_counts` | 3h |
| T17e | **TUI-4:** CDC Health spill-risk badge — match `cdc_trends` to source rows | 1h |
| T17f | **TUI-5:** Workers Interference sub-tab — `workers_tab` state, `Tab` cycling, interference table | 3h |
| T17g | **TUI-6:** Workers scheduler overhead bar — shared helper from diagnostics, one-line render | 1h |
| T17h | **TUI-7:** Dependencies Mermaid overlay — `x` key, `FetchExplainDag` action, scrollable overlay, `Ctrl+E` export | 2h |
| T17i | **TUI-8:** Header dog-feeding status badge — `df:N/M`, color logic, retention warning | 1h |
| T17j | **TUI-9:** Command palette `dog-feeding enable/disable/status` — ActionRequest variants, execution, confirmation | 2h |
| T17k | **TUI-10:** Detail view anomaly summary row in Properties section | 1h |
| T17l | **TUI-11:** Refresh Log `[auto]` tag for `initiated_by = 'DOG_FEED'` | 1h |
| T17m | **TUI-12:** First-launch dog-feeding toast — `shown_df_hint` flag, 10s expiry | 1h |
| T17n | **TUI-13:** Anomaly signals in `detect_issues()` — severity mapping, category "Anomaly" | 1h |
| T17o | **TUI-14:** Alert view `dog_feed_anomaly` event styling — cyan `🔍` icon | 30m |
| T17p | **TUI-16:** Diagnostics efficiency panel — aggregate speedup, diff/full counts | 2h |
| T17q | Update help overlay for Anomaly view, Workers tabs, Graph `x` key, header badge | 1h |
| T17r | **Snapshot tests (TUI-T1):** 22 new snapshot tests per test plan table above | 4h |
| T17s | **TUI-15:** Dashboard snapshot tests (5 snapshots: standard, wide, empty, anomalies, narrow) | 2h |
| T17t | **Unit tests:** 10 new unit tests per unit test plan table above | 2h |

**Exit:** All 16 TUI items implemented. Anomaly view renders. Dashboard badge
shows. CDC sparklines visible. Workers has Interference tab. Header badge
reflects DF status. Command palette supports dog-feeding. All snapshot and
unit tests pass. `just lint` clean.

### Phase T18 — Backend Enhancements (Day 9–12)

**Goal:** Implement DF-21 through DF-24 in the PostgreSQL extension.

| Item | Description | Effort |
|------|-------------|--------|
| T18a | **DF-21:** Add `sla_breach_risk` boolean to `df_threshold_advice` query in `src/api/dog_feeding.rs` | 2h |
| T18b | **DF-21:** E2E test asserting `sla_breach_risk = true` with synthetic data where `avg_diff_ms > freshness_deadline_ms` | 1h |
| T18c | **DF-22:** Implement `full` mode in `dog_feeding_auto_apply_tick()`: read interference data, widen dispatch for high-overlap pairs | 4h |
| T18d | **DF-22:** E2E test verifying dispatch interval widened after synthetic interference insertion | 2h |
| T18e | **DF-23:** Add `retention_warning` column to `dog_feeding_status()` output; check `history_retention_days` vs minimum window | 2h |
| T18f | **DF-23:** E2E test setting retention below minimum and verifying warning | 1h |
| T18g | **DF-24:** Modify `recommend_refresh_mode()` to read from `df_threshold_advice` when dog-feeding active | 3h |
| T18h | **DF-24:** E2E test verifying consistent results between `recommend_refresh_mode()` and `df_threshold_advice` | 1h |
| T18i | **TEST-21:** Proptest for `df_threshold_advice` bounds — 10k cases verifying `[0.01, 0.80]` clamping | 2h |
| T18j | Update upgrade SQL script `pg_trickle--0.20.0--0.21.0.sql` with new/altered functions | 2h |

**Exit:** All 4 backend enhancements implemented with E2E tests. Proptest
passes. Upgrade path works. `just lint` clean.

### Phase T19 — CLI Integration (Day 12–13)

**Goal:** Add CLI subcommands for dog-feeding lifecycle and graph export.

| Item | Description | Effort |
|------|-------------|--------|
| T19a | **CLI-1:** Add `DogFeeding` variant to `Commands` enum in `cli.rs` with `enable`, `disable`, `status` subcommands | 1h |
| T19b | **CLI-1:** Implement `commands/dog_feeding.rs` using `commands/domain.rs` shared logic | 2h |
| T19c | **CLI-1:** Wire dispatch in `main.rs` | 30m |
| T19d | **CLI-1:** `--format json|table|csv` for `status` subcommand | 1h |
| T19e | **CLI-2:** Add `--format ascii|mermaid|dot` flag to `GraphArgs` in `cli.rs` | 30m |
| T19f | **CLI-2:** Implement mermaid/dot paths in `commands/graph.rs` using `domain::export_dag()` | 1h |
| T19g | **Tests:** CLI parsing tests for new subcommands and flags | 1h |
| T19h | **Tests:** Integration tests for `dog-feeding enable/disable/status` against stubs | 1h |
| T19i | **Tests:** Integration tests for `graph --format mermaid` and `--format dot` | 1h |

**Exit:** `pgtrickle dog-feeding enable/disable/status` works.
`pgtrickle graph --format mermaid` outputs valid Mermaid. All tests pass.

### Phase T20 — Documentation, Polish & Final Testing (Day 13–15)

**Goal:** Documentation updates, cross-cutting tests, coverage audit,
final polish.

#### T20-A: Documentation (Day 13)

| Item | Description | Effort |
|------|-------------|--------|
| T20a-1 | **TUI-D1:** Update `docs/TUI.md` — new Anomaly view, CDC sparklines, Workers interference, Mermaid export, header badge, command palette commands | 2h |
| T20a-2 | **DOC-21:** Update `docs/GETTING_STARTED.md` Day 2 section — dog-feeding CLI and TUI integration | 1h |
| T20a-3 | **DOC-22:** Update `docs/SQL_REFERENCE.md` — `df_threshold_advice.sla_breach_risk` column | 30m |
| T20a-4 | Update `docs/CONFIGURATION.md` — `dog_feeding_auto_apply = 'full'` mode now implemented | 30m |
| T20a-5 | Update CHANGELOG.md with all v0.21.0 features | 1h |

#### T20-B: Cross-Cutting Tests (Day 14)

| Item | Description | Effort |
|------|-------------|--------|
| T20b-1 | **All views with dog-feeding off:** Verify every view renders correctly when `dog_feeding.enabled = false`; no panics, no visual artifacts | 1h |
| T20b-2 | **All views with dog-feeding on:** Full `sample_state_dog_feeding()` render pass; all panels populated | 1h |
| T20b-3 | **Narrow terminal (60×20):** All 15 views (incl. Anomalies) render without panics | 1h |
| T20b-4 | **View switching:** Verify subscriptions update when switching views; no stale data after switching from CDC → Dashboard | 1h |
| T20b-5 | **Reconnect with dog-feeding:** Verify dog-feeding data clears on disconnect and repopulates on reconnect | 1h |
| T20b-6 | **Action lifecycle:** `dog-feeding enable` → poll → verify `df:5/5` badge → anomalies appear → `dog-feeding disable` → confirm → verify `df:off` | 1h |

#### T20-C: Coverage Audit (Day 14–15)

| Item | Description | Effort |
|------|-------------|--------|
| T20c-1 | Run `cargo tarpaulin` / `cargo llvm-cov` on TUI crate | 30m |
| T20c-2 | Review uncovered lines in `state.rs` — target ≥85% | 1h |
| T20c-3 | Review uncovered lines in `poller/` — target ≥70% | 1h |
| T20c-4 | Review uncovered view rendering paths — target ≥80% | 1h |
| T20c-5 | Add missing tests for uncovered paths | 2h |

#### T20-D: Final Polish (Day 15)

| Item | Description | Effort |
|------|-------------|--------|
| T20d-1 | `just fmt && just lint` — zero warnings | 30m |
| T20d-2 | `insta review` — accept all new/changed snapshots | 30m |
| T20d-3 | Run full test suite: `just test-unit && just test-integration` | 1h |
| T20d-4 | Build E2E image and run: `just test-e2e` | 30m |
| T20d-5 | Manual smoke test: connect to pg_trickle instance, verify all 16 TUI items + 2 CLI commands end-to-end | 1h |
| T20d-6 | Verify `just check-version-sync` passes | 15m |
| T20d-7 | Verify upgrade path: `0.20.0 → 0.21.0` E2E test | 30m |

**Exit:** All documentation updated. All cross-cutting tests pass. Coverage
targets met. Full test suite green. Manual smoke test verified. `just lint`
clean. Version sync passes. Upgrade path verified.

---

## Exit Criteria

### Feature Completeness

- [ ] **T15: Architecture** — AppState uses domain structs; poller respects subscriptions; fetchers/updaters extracted
- [ ] **TUI-1:** Anomaly Detection view (`a`) shows rows from `df_anomaly_signals`; empty state with hint when off
- [ ] **TUI-2:** Dashboard ribbon shows `anomalies:N` badge in red when N > 0; hidden when 0
- [ ] **TUI-3:** CDC Health sparkline column populated from `df_cdc_buffer_trends`; `—` when off
- [ ] **TUI-4:** CDC Health `IMMINENT`/`ELEVATED` spill-risk badge from `check_cdc_health()`
- [ ] **TUI-5:** Workers `Interference` sub-tab shows `df_scheduling_interference` sorted by overlap count
- [ ] **TUI-6:** Workers scheduler overhead bar from `scheduler_overhead()`; greyed when < 5 cycles
- [ ] **TUI-7:** Dependencies `x` key opens Mermaid overlay; `Ctrl+E` exports; dog-feeding nodes green
- [ ] **TUI-8:** Header bar shows `df:5/5` (green), `df:off` (dim), or `df:⚠` (orange)
- [ ] **TUI-9:** Command palette `dog-feeding enable/disable` works; disable requires confirmation
- [ ] **TUI-10:** Detail view anomaly summary row from `df_anomaly_signals`; `—` when off
- [ ] **TUI-11:** Refresh Log `[auto]` tag on `initiated_by = 'DOG_FEED'` rows
- [ ] **TUI-12:** First-connect toast when dog-feeding not active; dismissed after 10 s
- [ ] **TUI-13:** Issues view surfaces anomaly signals as WARNING/CRITICAL entries
- [ ] **TUI-14:** Alerts view uses cyan `🔍` icon for `dog_feed_anomaly` events
- [ ] **TUI-15:** Dashboard snapshot tests cover standard, wide, empty, anomalies, narrow
- [ ] **TUI-16:** Diagnostics efficiency panel with aggregate speedup and diff/full counts
- [ ] **DF-21:** `df_threshold_advice.sla_breach_risk` column computed correctly
- [ ] **DF-22:** `dog_feeding_auto_apply = 'full'` widens dispatch for high-overlap pairs
- [ ] **DF-23:** `dog_feeding_status()` `retention_warning` set when retention too short
- [ ] **DF-24:** `recommend_refresh_mode()` reads from `df_threshold_advice` when DF active
- [ ] **CLI-1:** `pgtrickle dog-feeding enable/disable/status` with `--format json`
- [ ] **CLI-2:** `pgtrickle graph --format mermaid|dot` outputs valid Mermaid/DOT
- [ ] **TEST-21:** Proptest for threshold bounds passes 10k cases
- [ ] All dog-feeding panels degrade gracefully when dog-feeding not configured

### Test Coverage

- [ ] **Fixtures:** 6 new struct builders + `sample_state_dog_feeding()` in `test_fixtures.rs`
- [ ] **Unit tests:** ≥16 new unit tests (state, app, cli, poller subscriptions)
- [ ] **Snapshot tests:** ≥22 new snapshots covering all new/modified view panels
- [ ] **Contract stubs:** 9 new SQL stubs in `test_db.rs`
- [ ] **Contract tests:** 6 new `test_poll_df_*_executes` tests
- [ ] **Degradation tests:** 6 tests verifying graceful degradation when DF functions absent
- [ ] **CLI tests:** Parsing + integration tests for `dog-feeding` and `graph --format`
- [ ] **Cross-cutting:** All views render with DF on and off; narrow terminal verified
- [ ] **Coverage:** `state.rs` ≥85%, `poller/` ≥70%, `views/` ≥80%

### Documentation & CI

- [ ] `docs/TUI.md` updated with all new views, keybindings, panels
- [ ] `docs/GETTING_STARTED.md` Day 2 section includes dog-feeding CLI and TUI
- [ ] `docs/SQL_REFERENCE.md` documents `sla_breach_risk` column
- [ ] `docs/CONFIGURATION.md` updates `auto_apply = 'full'` as implemented
- [ ] CHANGELOG.md includes all features
- [ ] `just lint` zero warnings; `just test-all` green
- [ ] Extension upgrade path tested (`0.20.0 → 0.21.0`)
- [ ] `just check-version-sync` passes

---

## References

- [PLAN_TUI.md](PLAN_TUI.md) — v0.14.0 TUI foundation (T1–T8)
- [PLAN_TUI_PART_2.md](PLAN_TUI_PART_2.md) — v0.15.0 TUI improvements (T9–T14)
- [ROADMAP.md](../../ROADMAP.md) — v0.21.0 milestone definition
- [plans/PLAN_DOG_FEEDING.md](../PLAN_DOG_FEEDING.md) — Dog-feeding design plan
- [docs/TUI.md](../../docs/TUI.md) — TUI user documentation
- [docs/SQL_REFERENCE.md](../../docs/SQL_REFERENCE.md) — SQL API reference
- [docs/CONFIGURATION.md](../../docs/CONFIGURATION.md) — GUC reference
