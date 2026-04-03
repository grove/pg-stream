# PLAN_TUI_PART_2.md — TUI Improvements (v0.15)

**Milestone:** v0.15.0
**Status:** In Progress
**Effort:** ~10–12 days
**Last updated:** 2026-04-03
**Crate:** `pgtrickle-tui`
**Depends on:** [PLAN_TUI.md](PLAN_TUI.md) (T1–T8 implemented)

---

## Overview

PLAN_TUI.md delivered a comprehensive read-only TUI dashboard with 14 views,
18 CLI subcommands, and real-time LISTEN/NOTIFY alerts. Several high-value
features from the original plan were deferred — this document is the
implementation plan for the next wave of improvements.

The theme of this phase is **operational capability**: turning the TUI from a
monitoring dashboard into a tool operators can use to *act* on problems,
not just observe them.

### Implementation Status

| Feature | Priority | Status |
|---------|----------|--------|
| In-TUI write actions (refresh, pause, resume, fuse reset) | P0 | ✅ Implemented |
| Poller auto-reconnect with exponential backoff | P0 | ✅ Implemented |
| Command palette (`:` mode with fuzzy matching) | P1 | ✅ Implemented |
| Scrollable views (PgUp/PgDn, Home/End) | P1 | ✅ Implemented (PgUp/PgDn/Home/End) |
| Configurable poll interval (`--interval`) | P1 | ✅ Implemented |
| TLS support (`--sslmode`) | P1 | ✅ Implemented (behind `tls` feature flag) |
| Sort column cycling (`s`/`S`) | P2 | ✅ Implemented |
| Inline delta SQL fetch | P2 | ✅ Implemented (action + cache + inline view) |
| Cross-view filter persistence | P2 | ✅ Implemented (all 10 list views) |
| Toast / flash messages | P2 | ✅ Implemented |
| Parallel polling | P2 | ✅ Implemented (tokio::join! for all 17 queries) |
| Mouse support (`--mouse`) | P3 | ✅ Implemented (scroll wheel) |
| Theme toggle (`t` key, `--theme`) | P3 | ✅ Implemented |
| Export current view (`Ctrl+E`) | P3 | ✅ Implemented |
| Notification bell (`--bell`) | P3 | ✅ Implemented |
| Diagnostics signal breakdown | P1 | ✅ Implemented (signals polled + bar chart view) |
| Error diagnosis panel | P1 | ✅ Implemented (on-demand fetch + Detail view panel) |
| Source gating actions | P1 | ✅ Implemented (gate/ungate actions + Watermarks tab 2) |
| Dedup stats view | P2 | ✅ Implemented (polling + CDC view rendering) |
| Shared buffer stats view | P2 | ✅ Implemented (polling + CDC view table) |
| Explain refresh mode | P2 | ✅ Implemented (on-demand fetch + Detail Properties) |
| Source table detail | P2 | ✅ Implemented (on-demand fetch + Detail Sources section) |
| Diamond groups & SCC | P2 | ✅ Implemented (polling + Graph/Detail badges) |
| Quick health in header | P2 | ✅ Implemented (scheduler indicator) |
| CDC health deep dive | P2 | ✅ Implemented (polling + CDC view + Detail view) |
| Refresh history detail | P2 | ✅ Implemented (on-demand fetch + rich Detail history) |
| Repair action | P2 | ✅ Implemented (command palette + action) |
| Export DDL inline | P2 | ✅ Implemented (DDL overlay) |
| Query validation | P2 | ✅ Implemented (validate command) |
| Auxiliary columns view | P3 | ✅ Implemented (on-demand fetch + Delta Inspector tab) |

---

## Overview

PLAN_TUI.md delivered a comprehensive read-only TUI dashboard with 14 views,
18 CLI subcommands, and real-time LISTEN/NOTIFY alerts. Several high-value
features from the original plan were deferred — this document is the
implementation plan for the next wave of improvements.

The theme of this phase is **operational capability**: turning the TUI from a
monitoring dashboard into a tool operators can use to *act* on problems,
not just observe them.

### What This Plan Covers

**Operational capability (existing from initial draft):**

1. **In-TUI write actions** — refresh, pause/resume, fuse reset (P0)
2. **Connection resilience** — poller auto-reconnect with backoff (P0)
3. **Command palette** — fuzzy-search command execution (P1)
4. **Scrollable views** — `TableState` + viewport tracking (P1)
5. **Configurable poll interval** — `--interval` flag (P1)
6. **TLS support** — `--sslmode` for production connections (P1)
7. **Sort column cycling** — user-selectable sort order (P2)
8. **Inline delta SQL fetch** — show actual SQL in delta inspector (P2)
9. **Cross-view filter persistence** — filter applies to all list views (P2)
10. **Toast / flash messages** — transient status feedback (P2)
11. **Parallel polling** — `tokio::join!` for independent queries (P2)
12. **Mouse support** — click-to-select, scroll wheel (P3)
13. **Theme toggle** — light theme + `t` key / `--theme` flag (P3)
14. **Export current view** — `Ctrl+E` dump to JSON/CSV (P3)
15. **Notification bell** — terminal bell on critical alerts (P3)

**SQL API surface expansion (new):**

16. **Diagnostics signal breakdown** — render `recommend_refresh_mode().signals` JSONB as bar charts (P1)
17. **Dedup stats view** — surface `pgtrickle.dedup_stats()` in TUI (P2)
18. **Shared buffer stats view** — surface `pgtrickle.shared_buffer_stats()` in CDC view (P2)
19. **Error diagnosis panel** — surface `pgtrickle.diagnose_errors()` in Detail view (P1)
20. **Query validation** — `validate_query` via command palette (P2)
21. **Explain refresh mode** — show configured vs effective mode in Detail (P2)
22. **Source table detail** — surface `pgtrickle.list_sources()` in Detail view (P2)
23. **Diamond groups & SCC** — surface `diamond_groups()` and `pgt_scc_status()` in Graph/Detail (P2)
24. **Source gating actions** — gate/ungate from TUI via Watermarks view (P1)
25. **Quick health summary** — surface `pgtrickle.quick_health` in header bar (P2)
26. **Auxiliary columns view** — surface `list_auxiliary_columns()` in Delta Inspector (P3)
27. **CDC health deep dive** — surface `check_cdc_health()` in CDC view (P2)
28. **Refresh history detail** — surface `get_refresh_history()` with rows_inserted/deleted in Detail (P2)
29. **Repair action** — `repair_stream_table` from command palette and Fuse view (P2)
30. **Export DDL** — surface `export_definition()` inline from Detail view (P2)

### What Is NOT in Scope

- Batch multi-select operations (F17) — deferred to v0.16
- Config file (`~/.config/pgtrickle/config.toml`) — deferred
- Bookmarks and column picker — deferred
- Web UI — out of scope
- Multi-database switching — out of scope

---

## Table of Contents

- [Current State Analysis](#current-state-analysis)
- [Test Strategy](#test-strategy)
- [Feature Specifications](#feature-specifications)
  - [P0 — In-TUI Write Actions](#p0--in-tui-write-actions)
  - [P0 — Poller Auto-Reconnect](#p0--poller-auto-reconnect)
  - [P1 — Command Palette](#p1--command-palette)
  - [P1 — Scrollable Views with TableState](#p1--scrollable-views-with-tablestate)
  - [P1 — Configurable Poll Interval](#p1--configurable-poll-interval)
  - [P1 — TLS Support](#p1--tls-support)
  - [P2 — Sort Column Cycling](#p2--sort-column-cycling)
  - [P2 — Inline Delta SQL Fetch](#p2--inline-delta-sql-fetch)
  - [P2 — Cross-View Filter Persistence](#p2--cross-view-filter-persistence)
  - [P2 — Toast / Flash Messages](#p2--toast--flash-messages)
  - [P2 — Parallel Polling](#p2--parallel-polling)
  - [P3 — Mouse Support](#p3--mouse-support)
  - [P3 — Theme Toggle](#p3--theme-toggle)
  - [P3 — Export Current View](#p3--export-current-view)
  - [P3 — Notification Bell](#p3--notification-bell)
- [SQL API Surface Expansion](#sql-api-surface-expansion)
  - [P1 — Diagnostics Signal Breakdown](#p1--diagnostics-signal-breakdown)
  - [P1 — Error Diagnosis Panel](#p1--error-diagnosis-panel)
  - [P1 — Source Gating Actions](#p1--source-gating-actions)
  - [P2 — Dedup Stats View](#p2--dedup-stats-view)
  - [P2 — Shared Buffer Stats in CDC View](#p2--shared-buffer-stats-in-cdc-view)
  - [P2 — Explain Refresh Mode in Detail](#p2--explain-refresh-mode-in-detail)
  - [P2 — Source Table Detail](#p2--source-table-detail)
  - [P2 — Diamond Groups & SCC Status](#p2--diamond-groups--scc-status)
  - [P2 — Quick Health in Header Bar](#p2--quick-health-in-header-bar)
  - [P2 — CDC Health Deep Dive](#p2--cdc-health-deep-dive)
  - [P2 — Refresh History Detail](#p2--refresh-history-detail)
  - [P2 — Repair Action](#p2--repair-action)
  - [P2 — Export DDL Inline](#p2--export-ddl-inline)
  - [P2 — Query Validation](#p2--query-validation)
  - [P3 — Auxiliary Columns View](#p3--auxiliary-columns-view)
- [Implementation Phases](#implementation-phases)
- [Exit Criteria](#exit-criteria)

---

## Current State Analysis

The TUI as of v0.14.x has these gaps relative to the original PLAN_TUI.md:

| Original Feature | Status | Gap |
|-------------------|--------|-----|
| F1 Dashboard | ✅ Implemented | No bookmarks, column picker, grouping, row preview pane |
| F2 Detail View | ✅ Implemented | No defining query pane, table size/rows, SCC indicators |
| F3 Graph View | ✅ Basic | No path highlighting, broken edge rendering, staleness heatmap |
| F4 Refresh Log | ✅ Basic | Not scrollable past visible area; no filter |
| F5 Diagnostics | ✅ Basic | No signal breakdown bar charts, no apply action |
| F7 Config | ✅ Browse | No inline edit / ALTER SYSTEM |
| F8 Alerts | ✅ Basic | No toast popups for critical events |
| F9 Command Palette | ❌ Not started | Documented in help overlay but not wired |
| F14 Fuse | ✅ Display | No inline reset execution (A/R/S) |
| F15 Watermarks | ✅ Display | No gate/ungate from TUI |
| F16 Delta Inspector | ✅ Info-only | Delegates to CLI; no inline SQL |
| F17 Batch Ops | ❌ Not started | Multi-select, bulk actions |
| Theming | ✅ Dark only | No light theme, no user config |
| Adaptive polling | ❌ Not started | Fixed 2s interval |
| Mouse | ❌ Not started | No mouse events processed |
| Reconnect | ❌ Partial | LISTEN task retries; poller connects once, never reconnects |
| TLS | ❌ Not started | `NoTls` hardcoded |

### Code Architecture Notes

Key files to modify (all under `pgtrickle-tui/src/`):

| File | Lines | Role | Changes Needed |
|------|-------|------|----------------|
| `app.rs` | ~650 | Event loop, key handling, draw dispatch | Write actions, command palette, toast, mouse, reconnect channel |
| `state.rs` | ~500 | Data models, cascade/issue computation | Toast queue, sort state, filter scope |
| `poller.rs` | ~400 | 12 sequential DB queries | Parallel polling, reconnect logic, delta SQL fetch |
| `connection.rs` | ~90 | `tokio-postgres` connect | TLS support, reconnect helper |
| `cli.rs` | ~100 | clap definitions | `--interval`, `--theme`, `--sslmode`, `--mouse`, `--bell` flags |
| `theme.rs` | ~80 | Single dark theme | Add light theme, toggle mechanism |
| `views/dashboard.rs` | ~450 | Dashboard rendering | Sort indicators, TableState scroll |
| `views/delta_inspector.rs` | ~70 | Info-only panel | Inline SQL display |
| `views/help.rs` | ~130 | Help overlay | Update for new keybindings |

---

## Test Strategy

The TUI crate has four existing test tiers. Every new feature in this plan
must have coverage across the applicable tiers. This section defines the
strategy, and each implementation phase includes concrete test items.

### Test Tiers

| Tier | Location | Infra | Purpose |
|------|----------|-------|---------|
| **Unit** | `#[cfg(test)]` modules in `state.rs`, `app.rs`, `connection.rs`, `theme.rs`, `cli.rs`, `output.rs` | Pure Rust, no DB | Logic, counters, key dispatch, parsing |
| **Snapshot** | `views/mod.rs → snapshot_tests` | `ratatui::TestBackend` + `insta` | Visual regression for every view rendering |
| **Contract** | `command_tests.rs` + `test_db.rs` | Testcontainers (Postgres 18) | Poller SQL stubs match real extension signatures |
| **Integration** | `command_tests.rs` (execute paths) | Testcontainers (Postgres 18) | CLI command execute() succeeds against stubs |

### New State Structs Requiring Fixture Builders

Every new struct added to `state.rs` must have a corresponding builder in
`test_fixtures.rs` to enable snapshot and unit tests without a database.

| Struct | Fixture Builder | Used By |
|--------|-----------------|---------|
| `DiagSignals` (or extend `DiagRecommendation`) | `diag_with_signals(name, signals_json)` | Diagnostics signal bar chart |
| `DiagnosedError` | `diagnosed_error(time, error_type, msg, remediation)` | Error Diagnosis panel |
| `DedupStats` | `dedup_stats(total, needed, ratio)` | CDC dedup summary |
| `SharedBufferInfo` | `shared_buffer(source, consumers, rows)` | CDC buffer detail |
| `SourceGate` | `source_gate(source, gated, duration)` | Watermarks source gate tab |
| `WatermarkAlignment` | `watermark_alignment(group, lag, aligned)` | Watermarks enhanced tab |
| `DiamondGroup` | `diamond_group(id, member, is_convergence)` | Graph/Detail diamond badges |
| `SccGroup` | `scc_group(id, members, iterations)` | Graph/Detail SCC badges |
| `SourceTableInfo` | `source_info(source, cdc_mode, columns)` | Detail source table section |
| `CdcHealthEntry` | `cdc_health(source, mode, slot, lag, alert)` | CDC health deep dive |
| `QuickHealthSummary` | `quick_health(total, errors, stale, scheduler)` | Header scheduler indicator |
| `AuxiliaryColumn` | `aux_column(name, type_, purpose)` | Delta Inspector internals tab |
| `RefreshHistoryEntry` | `refresh_history(action, status, inserted, deleted, delta, duration)` | Detail refresh history |
| `ExplainRefreshMode` | `explain_mode(configured, effective, reason)` | Detail mode explanation |

### Extended `sample_state()` vs Separate Builders

The existing `sample_state()` already provides a rich AppState. For the new
fields, add a `sample_state_full()` that includes all SQL API data (signals,
dedup, shared buffers, source gates, diamond/SCC, CDC health, quick health,
etc.). This avoids breaking existing snapshot tests while providing data for
new tests.

```rust
/// sample_state() — existing (unchanged, avoids snapshot churn)
/// sample_state_full() — NEW: includes all SQL API surface data
pub fn sample_state_full() -> AppState {
    let mut state = sample_state();
    state.dedup_stats = Some(dedup_stats(1234, 87, 7.1));
    state.shared_buffers = vec![
        shared_buffer("orders", "order_totals, big_customers", 42),
        shared_buffer("sales", "revenue_daily", 1247),
    ];
    state.source_gates = vec![
        source_gate("public.events", true, Some("2h 15m")),
        source_gate("public.orders", false, None),
    ];
    state.cdc_health = vec![
        cdc_health("orders", "trigger", None, None, None),
        cdc_health("events", "wal", Some("pg_trickle_slot_16390"), Some(524288), None),
        cdc_health("logs", "transitioning", Some("pg_trickle_slot_16392"), Some(1258291), Some("lag")),
    ];
    state.diamond_groups = vec![
        diamond_group(1, "order_totals", false),
        diamond_group(1, "big_customers", true),
    ];
    state.scc_groups = vec![];  // No cycles in sample data
    state.quick_health = Some(quick_health(5, 1, 1, true));
    // Extend diagnostics with signals
    state.diagnostics[0].signals = Some(serde_json::json!({
        "change_ratio_current": {"score": 0.82, "weight": 0.25},
        "change_ratio_avg": {"score": 0.95, "weight": 0.30},
        "empirical_timing": {"score": 0.71, "weight": 0.35},
        "query_complexity": {"score": 0.35, "weight": 0.10},
        "target_size": {"score": 0.58, "weight": 0.10},
        "index_coverage": {"score": 1.00, "weight": 0.05},
        "latency_variance": {"score": 0.40, "weight": 0.05}
    }));
    state
}
```

### SQL Stub Extension for Contract Tests

`test_db.rs` contains `STUB_SQL` with all stub function definitions. For each
new SQL function polled, a matching stub must be added. Stubs must have
**exactly matching column names and types** — this is the whole point of
contract tests.

New stubs required:

| SQL Function | Returns | Stub Data |
|---|---|---|
| `dedup_stats()` | `total_diff_refreshes bigint, dedup_needed bigint, dedup_ratio_pct float8` | `1234, 87, 7.05` |
| `shared_buffer_stats()` | `source_oid oid, source_table text, consumer_count int, consumers text, columns_tracked int, safe_frontier_lsn text, buffer_rows bigint, is_partitioned bool` | One populated row |
| `check_cdc_health()` | `source_table text, cdc_mode text, slot_name text, lag_bytes bigint, confirmed_lsn text, alert text` | One trigger row + one wal row |
| `diagnose_errors(text)` | `event_time text, error_type text, error_message text, remediation text` | Two error rows |
| `validate_query(text)` | `check_name text, result text, severity text` | Two info rows |
| `explain_refresh_mode(text)` | `configured_mode text, effective_mode text, downgrade_reason text` | One row with downgrade |
| `list_sources(text)` | `source_table text, source_oid oid, source_type text, cdc_mode text, columns_used text` | Two rows |
| `diamond_groups()` | `group_id int, member_name text, member_schema text, is_convergence bool, epoch bigint, schedule_policy text` | Two rows in one group |
| `pgt_scc_status()` | `scc_id int, member_count int, members text, last_iterations int, last_converged_at text` | Zero rows (no cycles in test) |
| `bootstrap_gate_status()` | `source_table text, schema_name text, gated bool, gated_at text, ungated_at text, gated_by text, gate_duration text, affected_stream_tables text` | One gated + one ungated |
| `watermark_status()` | `group_name text, min_watermark text, max_watermark text, lag_secs float8, aligned bool, sources_with_watermark int, sources_total int` | One aligned + one lagging |
| `quick_health` (view) | `total_stream_tables int, error_tables int, stale_tables int, scheduler_running bool, status text` | One OK row |
| `list_auxiliary_columns(text)` | `column_name text, data_type text, purpose text` | Two rows |
| `get_refresh_history(text, int)` | `action text, status text, rows_inserted bigint, rows_deleted bigint, delta_row_count bigint, duration_ms float8, was_full_fallback bool, start_time text, error_message text` | Three rows |
| `repair_stream_table(text)` | `void` | No-op |
| `gate_source(text)` | `void` | No-op |
| `ungate_source(text)` | `void` | No-op |

### Snapshot Test Plan

New snapshot tests to add per feature:

| Feature | Test Name | State | Terminal Size | Key Assertions |
|---------|-----------|-------|---------------|----------------|
| Diagnostics signals | `test_diagnostics_signal_breakdown_80x40` | `sample_state_full()`, selected row 0 | 80×40 | Bar chart visible, signal names/scores rendered |
| Diagnostics signals | `test_diagnostics_no_selection_80x24` | `sample_state_full()`, no selection | 80×24 | Signal panel hidden |
| Error diagnosis | `test_detail_error_with_diagnosis_80x40` | Error table + `diagnosed_errors` | 80×40 | Error lines with types, remediation hints |
| Error diagnosis | `test_detail_active_no_diagnosis_80x40` | Active table, no errors | 80×40 | No diagnosis panel |
| Source gating | `test_watermarks_gates_tab_100x30` | `sample_state_full()` | 100×30 | Gate table with source/gated/duration columns |
| Source gating | `test_watermarks_alignment_tab_100x30` | `sample_state_full()` | 100×30 | Alignment lag + ✓/✗ indicators |
| Dedup stats | `test_cdc_view_with_dedup_100x30` | Dedup stats populated | 100×30 | Ratio bar, threshold text |
| Dedup stats | `test_cdc_view_dedup_warning_100x30` | Ratio ≥10% | 100×30 | Red threshold warning |
| Shared buffer detail | `test_cdc_buffer_detail_selected_100x30` | Buffer row selected | 100×30 | Consumer list, columns, LSN |
| Explain refresh mode | `test_detail_downgraded_mode_80x40` | Configured≠effective | 80×40 | Yellow "↓ downgraded" + reason |
| Source table detail | `test_detail_sources_section_80x40` | Sources populated | 80×40 | Source table with CDC mode, columns |
| Diamond/SCC | `test_graph_diamond_markers_80x24` | Diamond groups in state | 80×24 | `◆` badges on nodes |
| Diamond/SCC | `test_detail_diamond_badge_80x40` | ST in diamond group | 80×40 | Diamond badge in Properties |
| Quick health header | `test_dashboard_scheduler_running_80x24` | scheduler=true | 80×24 | Green "⚙ scheduler" |
| Quick health header | `test_dashboard_scheduler_stopped_80x24` | scheduler=false | 80×24 | Red "✗ scheduler stopped" |
| CDC health | `test_cdc_health_section_100x30` | WAL sources with lag | 100×30 | Slot names, lag, alert column |
| Refresh history | `test_detail_rich_history_80x40` | History with rows_inserted/deleted | 80×40 | "+42 -3 (42 Δ)" format |
| Export DDL overlay | `test_detail_ddl_overlay_80x30` | DDL fetched into cache | 80×30 | DDL text visible in overlay |
| Query validation | `test_validate_overlay_80x30` | Validation results | 80×30 | Checks table, resolved mode |
| Auxiliary columns | `test_delta_inspector_aux_tab_80x24` | Aux columns populated | 80×24 | Column names, types, purposes |

### Unit Test Plan

| Module | Test Name | What It Verifies |
|--------|-----------|------------------|
| `state.rs` | `test_scheduler_stopped_creates_issue` | `detect_issues()` adds CRITICAL issue when `quick_health.scheduler_running = false` |
| `state.rs` | `test_dedup_ratio_above_threshold` | `detect_issues()` adds warning when `dedup_ratio_pct ≥ 10.0` |
| `state.rs` | `test_cdc_health_lag_creates_issue` | `detect_issues()` adds warning when `lag_bytes > threshold` |
| `state.rs` | `test_source_gates_filter_gated` | Helper to filter gated-only sources works correctly |
| `state.rs` | `test_diamond_group_members_for_table` | Lookup returns correct group members for a given ST name |
| `state.rs` | `test_scc_group_members_for_table` | Returns empty when table not in any SCC |
| `app.rs` | `test_key_e_in_detail_triggers_ddl` | `e` key in Detail view dispatches `FetchDdl` action |
| `app.rs` | `test_key_g_in_watermarks_toggles_gate` | `g` key dispatches `GateSource`/`UngateSource` |
| `app.rs` | `test_tab_in_watermarks_cycles_subtab` | Tab key cycles between watermarks and gates tabs |
| `app.rs` | `test_tab_in_delta_inspector_cycles` | Tab key cycles between delta SQL and auxiliary columns |
| `app.rs` | `test_validate_command_parse` | `:validate SELECT ...` correctly parsed by command palette |
| `app.rs` | `test_repair_command_parse` | `:repair <name>` correctly parsed |
| Diagnostics signals | `test_signal_bar_width_scales` | Bar width calculation is proportional to score × available width |
| Diagnostics signals | `test_signal_bar_empty_for_zero` | Score of 0.0 renders zero-width bar |
| Diagnostics signals | `test_signals_json_parse_invalid` | Invalid JSON gracefully returns None |
| Dedup stats | `test_dedup_ratio_color_green` | <5% → green |
| Dedup stats | `test_dedup_ratio_color_yellow` | 5–10% → yellow |
| Dedup stats | `test_dedup_ratio_color_red` | ≥10% → red |
| Error diagnosis | `test_error_type_color_mapping` | Each error type maps to correct color |
| Error diagnosis | `test_error_type_unknown_uses_default` | Unknown type uses dim style |

### Contract Test Plan (Testcontainers)

For each new SQL function polled, add to `test_db.rs` and `command_tests.rs`:

| Stub Function | Contract Test |
|---|---|
| `pgtrickle.dedup_stats()` | `test_poll_dedup_stats_executes` |
| `pgtrickle.shared_buffer_stats()` | `test_poll_shared_buffer_stats_executes` |
| `pgtrickle.check_cdc_health()` | `test_poll_cdc_health_executes` |
| `pgtrickle.diagnose_errors(text)` | `test_poll_diagnose_errors_executes` |
| `pgtrickle.validate_query(text)` | `test_cmd_validate_executes` |
| `pgtrickle.explain_refresh_mode(text)` | `test_poll_explain_refresh_mode_executes` |
| `pgtrickle.list_sources(text)` | `test_poll_list_sources_executes` |
| `pgtrickle.diamond_groups()` | `test_poll_diamond_groups_executes` |
| `pgtrickle.pgt_scc_status()` | `test_poll_scc_status_executes` |
| `pgtrickle.bootstrap_gate_status()` | `test_poll_source_gates_executes` |
| `pgtrickle.watermark_status()` | `test_poll_watermark_status_executes` |
| `pgtrickle.quick_health` | `test_poll_quick_health_executes` |
| `pgtrickle.list_auxiliary_columns(text)` | `test_poll_auxiliary_columns_executes` |
| `pgtrickle.get_refresh_history(text, int)` | `test_poll_refresh_history_executes` |
| `pgtrickle.repair_stream_table(text)` | `test_action_repair_executes` |
| `pgtrickle.gate_source(text)` | `test_action_gate_source_executes` |
| `pgtrickle.ungate_source(text)` | `test_action_ungate_source_executes` |

### Action Channel Tests

Write actions need dedicated tests that verify the full action lifecycle:

| Test | What It Verifies |
|------|------------------|
| `test_action_gate_requires_confirmation` | Gate action goes through y/n confirmation |
| `test_action_ungate_no_confirmation` | Ungate executes without confirmation |
| `test_action_repair_requires_confirmation` | Repair asks for confirmation |
| `test_action_result_shows_toast` | Successful action produces toast message |
| `test_action_error_shows_error_toast` | Failed action produces error toast |
| `test_action_gate_toggles_state` | Gate on gated source → ungate; ungate on ungated → gate |

### Graceful Degradation Tests

The TUI must work against older extension versions that lack newer SQL
functions. Each new poller should handle "function does not exist" errors
by leaving the state field empty rather than crashing.

| Test | What It Verifies |
|------|------------------|
| `test_poll_dedup_stats_graceful_missing` | Returns `None` when function doesn't exist |
| `test_poll_shared_buffers_graceful_missing` | Returns empty `Vec` when function doesn't exist |
| `test_poll_cdc_health_graceful_missing` | Returns empty `Vec` |
| `test_poll_diamond_groups_graceful_missing` | Returns empty `Vec` |
| `test_poll_quick_health_graceful_missing` | Returns `None`, header omits scheduler indicator |
| `test_render_cdc_without_dedup` | CDC view renders without dedup section when data is `None` |
| `test_render_detail_without_sources` | Detail view renders without sources when empty |
| `test_render_watermarks_without_gates` | Gates tab shows "No source gates" placeholder |

### Test Coverage Targets

| Module | Current Coverage (est.) | Target | Strategy |
|--------|------------------------|--------|----------|
| `state.rs` | ~70% | ≥85% | Add issue detection tests for new fields |
| `app.rs` | ~60% | ≥75% | Add key dispatch tests for new keybindings |
| `poller.rs` | ~40% (contract only) | ≥65% | Contract tests for all 17 new stubs + graceful degradation |
| `views/` | ~80% (snapshot) | ≥85% | Snapshot tests for every new/modified panel |
| `test_fixtures.rs` | N/A (support) | 100% builders | Every new struct has a fixture builder |
| `connection.rs` | ~90% | ~90% | No major changes |

---

## Feature Specifications

### P0 — In-TUI Write Actions

**Problem:** The help overlay advertises `r` (refresh selected), `R` (refresh
all), and other action keys, but none execute — the TUI is read-only.
Operators must switch to a psql session or the CLI to act on problems they
spot in the dashboard.

**Solution:** Add a write channel from the UI thread to the poller's DB
connection. Actions execute SQL via the existing `tokio-postgres` client and
report results as toast messages.

#### Design

```
App (UI thread)                    Poller task
─────────────                      ───────────
handle_key('r')
  ├─ action_tx.send(Refresh(name)) ──▶ recv action
  │                                     ├─ SELECT pgtrickle.refresh_stream_table($1)
  │                                     ├─ result_tx.send(Ok("Refreshed in 42ms"))
  │  ◀── toast_rx.recv() ◀─────────────┘
  └─ app.toast = Some(Toast { ... })
```

**New channel:** `mpsc::channel::<ActionRequest>` from UI → poller. The
poller drains this channel at the top of each poll cycle (and on force-poll).

**ActionRequest enum:**

```rust
enum ActionRequest {
    RefreshTable(String),          // pgtrickle.refresh_stream_table($1)
    RefreshAll,                    // pgtrickle.refresh_all_stream_tables()
    PauseTable(String),            // pgtrickle.alter_stream_table($1, status => 'paused')
    ResumeTable(String),           // pgtrickle.alter_stream_table($1, status => 'active')
    ResetFuse(String, String),     // pgtrickle.reset_fuse($1, $2) — strategy: rearm/reinit/skip
}
```

**Result feedback:** Actions send results back over a second channel as
`ActionResult { success: bool, message: String }`, displayed as a toast
(see P2 — Toast).

#### Keybindings

| Key | View | Action | Confirmation |
|-----|------|--------|--------------|
| `r` | Dashboard, Detail | Refresh selected ST | None (safe, idempotent) |
| `R` | Dashboard | Refresh all active STs | "Refresh all N tables? (y/n)" |
| `p` | Dashboard, Detail | Pause selected ST | "Pause {name}? (y/n)" |
| `P` | Dashboard, Detail | Resume selected ST | None |
| `A` | Fuse view | Re-arm fuse for selected ST | "Re-arm fuse for {name}? (y/n)" |
| `g` | Watermarks (tab 2) | Toggle gate/ungate source | Gate: y/n; Ungate: none |
| `e` | Detail | Show export DDL overlay | None |
| `Ctrl+R` | Delta Inspector | Reload delta SQL from DB | None |
| `Ctrl+E` | Any view | Export view data to JSON file | None |

**Confirmation dialog:** A single-line prompt in the footer area:
`⚠ Refresh all 12 tables? [y/n]`. Only `y` or `n` are accepted; all other
keys are ignored. Esc cancels.

```
┌─────────────────────────────────────────────────────────────────────┐
│ ⚠ Pause daily_revenue? [y/n]                                       │
└─────────────────────────────────────────────────────────────────────┘
```

#### Implementation Details

1. Add `ActionRequest` enum to `app.rs`.
2. Add `action_tx: mpsc::Sender<ActionRequest>` to `App` struct.
3. In `poller_task()`, add a `tokio::select!` branch for `action_rx.recv()`.
4. Execute the SQL, capture Ok/Err, send `ActionResult` back via a separate
   `result_tx` channel.
5. In the main event loop, drain `result_rx` and push to `app.toast`.
6. After any write action, trigger an immediate force-poll to update state.
7. Add `confirming: Option<ActionRequest>` to `App` for the confirmation
   dialog state machine.

#### Safety

- Write actions are only allowed when `state.connected == true`.
- `RefreshAll` requires explicit confirmation.
- `Pause` requires explicit confirmation.
- `Drop` is intentionally NOT available from the TUI — use CLI or psql.
- All SQL uses parameterized queries (no string interpolation).

---

### P0 — Poller Auto-Reconnect

**Problem:** The `poller_task()` connects once at startup. If the connection
drops (server restart, network blip, idle timeout), the poller silently
stops updating — the header shows `● connected` but data is stale. The
LISTEN task already has retry logic, but the main poller does not.

**Solution:** Wrap each poll cycle in error detection. On query failure,
mark state as disconnected, enter reconnect loop with exponential backoff,
and resume polling on success.

#### Design

```rust
async fn poller_task(conn_args, tx, force_rx, action_rx) {
    loop {
        // Connect (or reconnect)
        let client = loop {
            match connect(&conn_args).await {
                Ok(c) => break c,
                Err(e) => {
                    tx.send(PollMsg::Error(format!("Connecting: {e}"))).await;
                    backoff.wait().await;
                    if tx.is_closed() { return; }
                }
            }
        };

        // Poll loop (breaks on connection error)
        let mut tick = interval(Duration::from_secs(poll_interval));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = force_rx.recv() => {}
                action = action_rx.recv() => { /* execute action */ }
            }

            let mut state = AppState::default();
            poll_all(&client, &mut state).await;

            // Detect connection loss: if every poll sub-query failed,
            // assume the connection is dead.
            if state.all_polls_failed() {
                tx.send(PollMsg::Error("Connection lost".into())).await;
                break; // → outer reconnect loop
            }

            tx.send(PollMsg::StateUpdate(Box::new(state))).await;
        }

        // Backoff before reconnecting
        backoff.reset();
    }
}
```

#### Backoff Schedule

| Attempt | Delay |
|---------|-------|
| 1 | 1s |
| 2 | 2s |
| 3 | 4s |
| 4 | 8s |
| 5+ | 15s (cap) |

Reset to 1s after a successful poll cycle.

#### State Indicators

- `state.connected = false` → header shows `✗ disconnected` (red)
- `state.reconnecting = true` → header shows `◌ reconnecting…` (yellow)
- On reconnect → force immediate poll, header goes back to `● connected`

#### Implementation Details

1. Add `all_polls_failed()` to `AppState` — returns true when
   `error_message` is set and `stream_tables` is empty (all polls errored).
2. Restructure `poller_task()` with outer reconnect loop + inner poll loop.
3. Add a simple `ExponentialBackoff` struct (no external crate needed):
   ```rust
   struct Backoff { attempt: u32, max_delay: Duration }
   impl Backoff {
       fn wait(&mut self) -> Duration {
           let delay = Duration::from_secs(1 << self.attempt.min(4));
           self.attempt += 1;
           delay.min(self.max_delay)
       }
       fn reset(&mut self) { self.attempt = 0; }
   }
   ```
4. The LISTEN task already retries independently — no changes needed there.

---

### P1 — Command Palette

**Problem:** The help overlay documents `:` for a command palette, but it's
not implemented. Rare actions (alter schedule, reset fuse with specific
strategy, gate source) require switching to the CLI.

**Solution:** A single-line command input at the bottom of the screen,
triggered by `:`. Supports fuzzy matching, tab completion for stream table
names, and a fixed command vocabulary.

#### UI Mockup

```
╔═════════════════════════════════════════════════════════════════════╗
║                          (current view)                            ║
╠═════════════════════════════════════════════════════════════════════╣
║  : refresh ord█                                                    ║
║  ▸ refresh order_totals     Trigger manual refresh                 ║
║    refresh all              Refresh all active stream tables        ║
║    refresh order_archive    Trigger manual refresh                 ║
╚═════════════════════════════════════════════════════════════════════╝
```

#### Command Grammar

```
refresh <name>                 — Trigger manual refresh
refresh all                    — Refresh all active stream tables
pause <name>                   — Pause (set status = PAUSED)
resume <name>                  — Resume (set status = ACTIVE)
alter <name> mode=<mode>       — Change refresh mode
alter <name> schedule=<interval> — Change schedule
alter <name> tier=<tier>       — Change tier
fuse reset <name>              — Re-arm fuse (default strategy)
fuse reset <name> reinitialize — Re-arm + full refresh
fuse reset <name> skip         — Re-arm + discard pending changes
explain <name>                 — Show delta SQL (switches to delta inspector)
export <name>                  — Show DDL overlay (same as ‘e’ in Detail)
quit                           — Exit pgtrickle
```

#### Implementation Details

1. Add `CommandPalette` struct to `app.rs`:
   ```rust
   struct CommandPalette {
       input: String,
       cursor: usize,
       suggestions: Vec<CommandSuggestion>,
       selected_suggestion: usize,
       visible: bool,
   }
   ```
2. When `:` is pressed, set `palette.visible = true` and enter palette
   input mode (similar to existing filter input mode).
3. On each keystroke, re-compute suggestions by fuzzy-matching `input`
   against command names and ST names from `state.stream_tables`.
4. `Tab` accepts the top suggestion (fills input). `Enter` executes.
5. Command parsing: split input on whitespace, match first token to
   command, extract arguments. Errors shown as toast.
6. Commands that require confirmation (pause, drop) enter the confirmation
   dialog (reusing the P0 confirmation mechanism).
7. Drawing: render the palette as an overlay at the bottom of the screen,
   3–5 lines tall, over the footer bar.

#### Fuzzy Matching

Simple substring matching (same as the existing `/` filter). Future: upgrade
to `nucleo` or `fuzzy-matcher` crate for ranked fuzzy matches.

---

### P1 — Scrollable Views with TableState

**Problem:** Most views render all rows via `Paragraph` or `Table` without
scroll tracking. With 200+ stream tables or refresh log entries, content
overflows the visible area and users cannot reach rows past the bottom of
the screen.

**Solution:** Replace manual `selected: usize` tracking with ratatui's
`StatefulWidget<TableState>` for all table-based views. This gives automatic
viewport scrolling, offset management, and scroll indicators.

#### Affected Views

| View | Current Widget | Migration |
|------|---------------|-----------|
| Dashboard | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Refresh Log | `Paragraph` (no scroll) | `Table` + `TableState` |
| Diagnostics | `Table` + manual highlight | `StatefulWidget<TableState>` |
| CDC Health | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Config | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Health | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Workers | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Fuse | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Watermarks | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Issues | `Table` + manual highlight | `StatefulWidget<TableState>` |
| Graph | `Paragraph` + manual highlight | `Paragraph` with scroll offset |
| Alerts | `Paragraph` (no scroll) | `Paragraph` with scroll offset |

#### Design

1. Move `selected: usize` from `App` into per-view `TableState` instances
   stored in a `ViewStates` struct:
   ```rust
   struct ViewStates {
       dashboard: TableState,
       refresh_log: TableState,
       diagnostics: TableState,
       cdc: TableState,
       config: TableState,
       health: TableState,
       workers: TableState,
       fuse: TableState,
       watermarks: TableState,
       issues: TableState,
       // For non-table views:
       graph_offset: usize,
       alert_offset: usize,
   }
   ```
2. Each view's `render()` function takes `&mut TableState` and uses
   `frame.render_stateful_widget(table, area, &mut table_state)`.
3. Navigation keys (`j`/`k`/Home/End) call `table_state.select_next()`,
   `table_state.select_previous()`, etc.
4. ratatui handles viewport offset automatically — rows scroll into view
   when selected.

#### Scroll Indicators

Add a subtle `Scrollbar` widget to views with more rows than visible:

```rust
let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
    .begin_symbol(Some("↑"))
    .end_symbol(Some("↓"));
frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
```

#### Page Navigation

Add `PgUp`/`PgDn` keybindings for jumping by one visible page:

| Key | Action |
|-----|--------|
| `PgUp` / `Ctrl+U` | Move selection up by page height |
| `PgDn` / `Ctrl+D` | Move selection down by page height |

---

### P1 — Configurable Poll Interval

**Problem:** The 2-second poll interval is hardcoded in `poller_task()`.
In high-load production environments, 2s may be too aggressive; in debugging
scenarios, operators may want sub-second updates.

**Solution:** Add `--interval <seconds>` CLI flag (default: 2). Display the
active interval in the header bar.

#### CLI

```rust
/// Poll interval in seconds (default: 2)
#[arg(long, default_value = "2", env = "PGTRICKLE_POLL_INTERVAL")]
pub interval: u64,
```

#### Header Display

```
pg_trickle  Dashboard  ● connected  ⏱ 5s  polled 3s ago
```

The `⏱ 5s` segment shows the configured interval. `polled 3s ago` already
exists.

#### Implementation

1. Add `interval` field to `ConnectionArgs` (or a new `TuiArgs` struct).
2. Pass to `poller_task()` as a parameter.
3. Replace `interval(Duration::from_secs(2))` with
   `interval(Duration::from_secs(poll_interval))`.
4. Store `poll_interval` in `AppState` for display in the header.
5. Clamp: minimum 1s, maximum 300s. Values outside this range produce a
   clap validation error.

---

### P1 — TLS Support

**Problem:** `connection.rs` uses `tokio_postgres::NoTls`. Production
PostgreSQL deployments commonly require TLS. Users who need TLS must
currently use an intermediate proxy (e.g., pgbouncer with TLS termination).

**Solution:** Add `--sslmode` flag that enables TLS via `tokio-postgres-rustls`.

#### CLI

```rust
/// SSL mode: disable, prefer, require (default: prefer)
#[arg(long, default_value = "prefer", env = "PGSSLMODE")]
pub sslmode: SslMode,
```

#### Dependencies

Add to `pgtrickle-tui/Cargo.toml`:
```toml
tokio-postgres-rustls = "0.13"
rustls = { version = "0.23", default-features = false, features = ["ring"] }
webpki-roots = "0.26"
```

#### Connection Logic

```rust
pub async fn connect(args: &ConnectionArgs) -> Result<Client, CliError> {
    let conn_string = build_connection_string(args);

    match args.sslmode {
        SslMode::Disable => {
            let (client, conn) = tokio_postgres::connect(&conn_string, NoTls).await?;
            tokio::spawn(conn);
            Ok(client)
        }
        SslMode::Prefer | SslMode::Require => {
            let tls_config = rustls::ClientConfig::builder()
                .with_root_certificates(webpki_roots::TLS_SERVER_ROOTS.into())
                .with_no_client_auth();
            let tls = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);
            match tokio_postgres::connect(&conn_string, tls.clone()).await {
                Ok((client, conn)) => {
                    tokio::spawn(conn);
                    Ok(client)
                }
                Err(e) if args.sslmode == SslMode::Prefer => {
                    // Fall back to no TLS
                    let (client, conn) = tokio_postgres::connect(&conn_string, NoTls).await?;
                    tokio::spawn(conn);
                    Ok(client)
                }
                Err(e) => Err(e.into()),
            }
        }
    }
}
```

#### Environment Variable

Respects `PGSSLMODE` (matching libpq convention). Values: `disable`,
`prefer`, `require`. The `verify-ca` and `verify-full` modes are supported
when `--sslrootcert` is provided.

---

### P2 — Sort Column Cycling

**Problem:** Dashboard has a hardcoded sort (errors → cascade-stale → stale →
name). Users managing many tables may want to sort by avg duration, total
refreshes, or last refresh time to find performance outliers.

**Solution:** `s` key cycles through sort modes. A `▲`/`▼` indicator in the
column header shows the active sort.

#### Sort Modes

| Key press | Sort by | Direction |
|-----------|---------|-----------|
| `s` (1st) | Status severity (default) | Errors first |
| `s` (2nd) | Name | A→Z |
| `s` (3rd) | Avg duration | Slowest first |
| `s` (4th) | Last refresh | Oldest first |
| `s` (5th) | Total refreshes | Most first |
| `s` (6th) | Staleness | Stale first |
| `S` | Reverse current sort direction | Toggle ▲/▼ |

#### Implementation

1. Add `SortMode` enum and `sort_ascending: bool` to `App`.
2. In `filtered_sorted_indices()`, use `SortMode` instead of the hardcoded
   comparator.
3. Dashboard header row renders `▲` or `▼` next to the active sort column.
4. `switch_view()` to Dashboard resets sort to default (status severity).

---

### P2 — Inline Delta SQL Fetch

**Problem:** The Delta Inspector view (view `d`) currently shows only a
static message directing users to run `pgtrickle explain <name>` in a
separate terminal. This breaks the TUI's promise of keeping the operator
in a single interface.

**Solution:** Fetch the actual delta SQL from the database and display it
inline. Add syntax highlighting via simple keyword coloring.

#### Design

The delta SQL is fetched on-demand (not every poll cycle) when the user
navigates to the Delta Inspector view with a table selected.

1. Add `delta_sql_cache: HashMap<String, String>` to `AppState`.
2. When the Delta Inspector view is entered and the selected ST is not in
   the cache, send a `FetchDeltaSql(name)` request over the action channel.
3. The poller executes:
   ```sql
   SELECT pgtrickle.explain_delta($1)
   ```
4. Result stored in `delta_sql_cache` and sent back via `PollMsg`.
5. View renders the SQL with keyword highlighting:
   - `SELECT`, `FROM`, `WHERE`, `JOIN`, `GROUP BY`, `MERGE`, `INSERT`,
     `UPDATE`, `DELETE`, `WITH`, `AS`, `CASE`, `WHEN`, `THEN`, `END`,
     `SUM`, `COUNT`, `COALESCE` → cyan/bold
   - String literals → green
   - Numbers → yellow
   - Comments → dim grey

#### UI Mockup

```
╔════════════════════════════════════════════════════════════════════╗
║  Delta Inspector — public.order_totals                            ║
╠════════════════════════════════════════════════════════════════════╣
║                                                                    ║
║  WITH delta_orders AS (                                            ║
║    SELECT customer_id, amount, _pgt_op                             ║
║    FROM pgtrickle_changes.changes_16384                            ║
║    WHERE _pgt_change_seq > $1                                      ║
║  ),                                                                ║
║  agg_delta AS (                                                    ║
║    SELECT customer_id,                                             ║
║      SUM(CASE WHEN _pgt_op = 'I' THEN amount                      ║
║               WHEN _pgt_op = 'D' THEN -amount END) AS d_total,    ║
║      SUM(CASE WHEN _pgt_op = 'I' THEN 1                           ║
║               WHEN _pgt_op = 'D' THEN -1 END) AS d_cnt            ║
║    FROM delta_orders                                               ║
║    GROUP BY customer_id                                            ║
║  )                                                                 ║
║  MERGE INTO order_totals t USING agg_delta d                       ║
║  ON t.customer_id = d.customer_id ...                              ║
║                                                                    ║
║  Mode: DIFFERENTIAL  Avg: 42ms  Last: 38ms  Refreshes: 1,247      ║
║                                                                    ║
╠════════════════════════════════════════════════════════════════════╣
║  Ctrl+R Reload SQL  Esc Back  ? Help                               ║
╚════════════════════════════════════════════════════════════════════╝
```

The SQL body is scrollable. A stats bar at the bottom shows refresh metrics
for context. `Ctrl+R` reloads the SQL from the database (cache-bust).

---

### P2 — Cross-View Filter Persistence

**Problem:** The `/` filter currently only applies to Dashboard, Detail, and
DeltaInspector views (which share a stream-table list). Other views with
list data (Refresh Log, CDC, Issues, Fuse, Workers) ignore the filter.

**Solution:** Make the filter persist across view switches and apply to any
view that has a filterable list. Each view defines its own filter predicate.

#### Filter Scope

| View | Filter matches on |
|------|-------------------|
| Dashboard | name, schema, status, mode |
| Detail | (same as Dashboard — selects which ST) |
| Refresh Log | `st_name` |
| CDC Health | `stream_table`, `source_table` |
| Health Checks | `check_name`, `detail` |
| Workers | `table_name` |
| Fuse | `stream_table` |
| Issues | `affected_table`, `category`, `summary` |
| Config | `name`, `short_desc` |

Views that are not list-based (Graph, Alerts, Watermarks, DeltaInspector)
ignore the filter but preserve it — switching back to a list view re-applies
it.

#### Implementation

1. Keep `filter: Option<String>` in `App` (already exists).
2. Each view's `render()` call applies the filter to its own data. Add a
   `fn matches_filter(item: &T, filter: &str) -> bool` per view type.
3. Footer shows `/{filter}` in yellow when a filter is active (already
   exists, just needs to be visible from all views).
4. `Esc` in any non-Dashboard view clears the filter, **unless** an overlay
   (help, DDL, validate, command palette) is active — in that case `Esc`
   dismisses the overlay first. A second `Esc` then clears the filter.

---

### P2 — Toast / Flash Messages

**Problem:** After a manual refresh, force-poll, or any future write action,
there is no visible feedback beyond the data eventually updating on the next
poll cycle.

**Solution:** A transient message bar in the footer that auto-clears after
3 seconds. Shows success/error status with icon.

#### Design

```rust
struct Toast {
    message: String,
    style: ToastStyle,  // Success, Error, Info
    expires_at: Instant,
}

enum ToastStyle {
    Success,  // green ✓
    Error,    // red ✗
    Info,     // blue ●
}
```

```
┌─────────────────────────────────────────────────────────────────────┐
│ ✓ Refreshed order_totals — 42ms, 18 rows affected                  │
└─────────────────────────────────────────────────────────────────────┘
```

#### Rendering

The toast replaces the footer bar for its duration. The main event loop
checks `Instant::now() > toast.expires_at` to clear it. Multiple toasts
queue — only the latest is shown.

#### Implementation

1. Add `toast: Option<Toast>` to `App`.
2. In `draw_footer()`, if `toast.is_some()` and not expired, render the
   toast instead of the keybinding hints.
3. The expired check happens in the main event loop (already runs at 50ms).
4. Write action results (P0) push toasts. Force-poll pushes an info toast.
   Connection recovery pushes a success toast.

---

### P2 — Parallel Polling

**Problem:** `poll_all()` executes 12 SQL queries sequentially. On a
high-latency connection (100ms RTT), the poll takes 1.2s+ — more than half
the 2s interval is spent blocked on I/O. With the SQL API expansion, the
total will grow to ~20 polls.

**Solution:** Group all polls into independent batches and execute them
concurrently with `tokio::join!`.

#### Dependency Analysis

All 12 poll functions are independent — they read from different SQL
functions and write to different `AppState` fields. The only dependency is
that `compute_cascade_staleness()` and `detect_issues()` run after all polls
complete, which is already the case.

#### Implementation

```rust
pub async fn poll_all(client: &Client, state: &mut AppState) {
    state.connected = true;
    state.reconnecting = false;
    state.last_poll = Some(chrono::Utc::now());

    // Execute all independent polls concurrently.
    let (tables, health, cdc, dag, diag, eff, gucs, log, workers, fuses, wm, triggers) =
        tokio::join!(
            poll_stream_tables(client),
            poll_health(client),
            poll_cdc(client),
            poll_dag(client),
            poll_diagnostics(client),
            poll_efficiency(client),
            poll_gucs(client),
            poll_refresh_log(client),
            poll_workers(client),
            poll_fuses(client),
            poll_watermarks(client),
            poll_triggers(client),
        );

    // Merge results into state (each returns Option<T>).
    if let Some(t) = tables { state.stream_tables = t; }
    if let Some(h) = health { state.health_checks = h; }
    // ... etc

    state.compute_cascade_staleness();
    state.detect_issues();
}
```

Each `poll_*` function changes its signature from `(client, state)` to
`(client) -> Option<Vec<T>>`, returning `None` on error.

#### Expected Improvement

- Sequential (100ms RTT): 20 × 100ms = 2.0s
- Parallel (100ms RTT): ~100ms (single round trip with connection
  multiplexing, or up to 200ms with query execution time)
- On localhost: negligible difference (queries already fast)

Note: `tokio-postgres` uses a single TCP connection, so queries are
pipelined rather than truly parallel. The improvement comes from overlapping
network I/O and PostgreSQL query processing.

---

### P3 — Mouse Support

**Problem:** The TUI requires keyboard-only interaction. Users connecting
via SSH or unfamiliar with vim-style navigation find it difficult to select
rows or switch views.

**Solution:** Enable optional mouse support via `crossterm::event::EnableMouseCapture`.

#### Features

| Mouse Action | Result |
|-------------|--------|
| Left-click on table row | Select that row |
| Left-click on footer view tab | Switch to that view |
| Scroll wheel up/down | Navigate table selection |
| Left-click on issues sidebar item | Select issue |

#### Activation

- `--mouse` CLI flag or `PGTRICKLE_MOUSE=1` env var
- Disabled by default (mouse capture can interfere with terminal copy/paste)

#### Implementation

1. In `run()`, conditionally call `execute!(stdout, EnableMouseCapture)`.
2. In the event loop, handle `Event::Mouse(MouseEvent { kind, column, row })`.
3. Click-to-select: hit-test the `column, row` against rendered table
   areas to determine which row was clicked. This requires storing the
   rendered table's `Rect` and row count.
4. Scroll: map `MouseEventKind::ScrollUp` / `ScrollDown` to `move_up()` /
   `move_down()`.
5. On exit: `execute!(stdout, DisableMouseCapture)`.

#### Hit Testing

Store `last_table_area: Option<Rect>` and `last_table_rows: usize` in `App`.
Set after each `render()`. On click, compute:
```rust
let row_in_table = (click_y - table_area.y - header_height) as usize + scroll_offset;
```

---

### P3 — Theme Toggle

**Problem:** Only `Theme::default_dark()` exists. Users with light terminal
backgrounds get poor contrast.

**Solution:** Add `Theme::light()` and a `t` key to toggle between themes
at runtime. Also accept `--theme dark|light` CLI flag.

#### Light Theme Colors

```rust
pub fn light() -> Self {
    Self {
        name: "light",
        active: Style::default().fg(Color::DarkGreen),
        error: Style::default().fg(Color::Red),
        suspended: Style::default().fg(Color::DarkYellow),
        paused: Style::default().fg(Color::Gray),
        selected: Style::default()
            .bg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        header: Style::default()
            .fg(Color::DarkBlue)
            .add_modifier(Modifier::BOLD),
        border: Style::default().fg(Color::Gray),
        warning: Style::default().fg(Color::DarkYellow),
        ok: Style::default().fg(Color::DarkGreen),
        title: Style::default()
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        footer: Style::default().fg(Color::Gray),
        footer_active: Style::default()
            .fg(Color::Black)
            .bg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        dim: Style::default().fg(Color::Gray),
    }
}
```

#### Implementation

1. Add `Theme::light()` to `theme.rs`.
2. Add `--theme dark|light` to `ConnectionArgs` (or `TuiArgs`).
3. In `handle_key()`, `t` toggles `app.theme` between dark and light.
4. All views already take `&Theme` as a parameter — no view changes needed.

---

### P3 — Export Current View

**Problem:** An operator spots a problem in the Issues or CDC view and wants
to share it in Slack or a ticket — they must manually copy text from the
terminal.

**Solution:** `Ctrl+E` exports the current view's underlying data to stdout
(after restoring the terminal) or writes to a file.

#### Design

1. `Ctrl+E` triggers an export of the current view's data.
2. Format: JSON by default (matches CLI `--format json` output).
3. Output goes to a temporary file (`/tmp/pgtrickle_export_<timestamp>.json`)
   and a toast shows the file path.
4. Data source per view:
   - Dashboard → `state.stream_tables` (serializable)
   - Issues → `state.issues`
   - CDC → `state.cdc_buffers`
   - Health → `state.health_checks`
   - Fuse → `state.fuses`
   - Refresh Log → `state.refresh_log`
   - Config → `state.guc_params`
   - Workers → `state.workers` + `state.job_queue`
   - Watermarks → `state.watermark_groups`

#### Implementation

1. Add `#[derive(Serialize)]` to all state structs that don't have it yet
   (`RefreshLogEntry`, `DagEdge`, `AlertEvent`, `Issue`).
2. In `handle_key()`, `Ctrl+E` calls `export_current_view(&app)`.
3. `export_current_view` serializes the relevant data to JSON and writes
   to a tempfile. Returns the path as a toast message.
4. No terminal mode switching needed — just write to a file.

---

### P3 — Notification Bell

**Problem:** When the TUI is running in a background terminal tab, operators
miss critical alerts (blown fuses, refresh failures) until they happen to
glance at it.

**Solution:** Emit a terminal bell character (`\x07`) when a critical alert
arrives via LISTEN/NOTIFY. Most terminals can be configured to flash the
tab or play a sound on bell.

#### Activation

- `--bell` CLI flag or `PGTRICKLE_BELL=1` env var
- Disabled by default (to avoid surprising users)

#### Implementation

1. Add `bell_enabled: bool` to `App`.
2. When draining `alert_rx` in the main loop, if `alert.severity == "critical"`
   and `bell_enabled`, write `\x07` to stdout.
3. Rate-limit: max one bell per 10 seconds to avoid bell storms.

```rust
if app.bell_enabled && alert.severity == "critical" {
    if app.last_bell.elapsed() > Duration::from_secs(10) {
        print!("\x07");
        app.last_bell = Instant::now();
    }
}
```

---

## SQL API Surface Expansion

The following features surface SQL functions from `SQL_REFERENCE.md` that are
not yet visible in the TUI. This is ordered by impact — functions that help
operators diagnose and fix problems are prioritized.

### SQL API Coverage Audit

| SQL Function | Currently Polled | Currently Displayed | Gap |
|---|---|---|---|
| `st_refresh_stats()` | ✅ | ✅ Dashboard + Detail | — |
| `health_check()` | ✅ | ✅ Health view | — |
| `change_buffer_sizes()` | ✅ | ✅ CDC view | — |
| `dependency_tree()` | ✅ | ✅ Graph view | — |
| `recommend_refresh_mode()` | ✅ | ✅ Table only | No signal JSONB breakdown |
| `refresh_efficiency()` | ✅ | ✅ Detail view | — |
| `refresh_timeline()` | ✅ | ✅ Refresh Log | Missing rows_inserted/deleted |
| `worker_pool_status()` | ✅ | ✅ Workers view | — |
| `parallel_job_status()` | ✅ | ✅ Workers view | — |
| `fuse_status()` | ✅ | ✅ Fuse view | No inline reset |
| `watermark_groups()` | ✅ | ✅ Watermarks view | — |
| `trigger_inventory()` | ✅ | ✅ CDC view | — |
| **`explain_delta()`** | ❌ | ❌ Info-only panel | **Need inline fetch** |
| **`dedup_stats()`** | ❌ | ❌ Not surfaced | **New** |
| **`shared_buffer_stats()`** | ❌ | ❌ Not surfaced | **New** |
| **`diagnose_errors()`** | ❌ | ❌ Not surfaced | **New** |
| **`validate_query()`** | ❌ | ❌ Not surfaced | **New — via palette** |
| **`explain_refresh_mode()`** | ❌ | ❌ Not surfaced | **New — Detail panel** |
| **`list_sources()`** | ❌ | ❌ Not surfaced | **New — Detail panel** |
| **`diamond_groups()`** | ❌ | ❌ Not surfaced | **New — Graph/Detail** |
| **`pgt_scc_status()`** | ❌ | ❌ Not surfaced | **New — Graph/Detail** |
| **`check_cdc_health()`** | ❌ | ❌ Not surfaced | **New — CDC view** |
| ~~`slot_health()`~~ | ❌ | ❌ | Covered by `check_cdc_health()` |
| **`export_definition()`** | ❌ CLI only | ❌ Not in TUI | **New — Detail view** |
| **`repair_stream_table()`** | ❌ | ❌ Not surfaced | **New — action** |
| **`get_refresh_history()`** | ❌ (uses timeline) | Partial | **Richer Detail data** |
| **`quick_health`** (view) | ❌ | ❌ Not surfaced | **New — header enrichment** |
| **`list_auxiliary_columns()`** | ❌ | ❌ Not surfaced | **New — Delta Inspector** |
| **`source_gates()`** | ❌ | ❌ Not surfaced | Superseded by `bootstrap_gate_status()` |
| **`gate_source()` / `ungate_source()`** | ❌ | ❌ Not surfaced | **New — actions** |
| **`explain_query_rewrite()`** | ❌ | ❌ Not surfaced | Deferred (complex UI) |
| **`bootstrap_gate_status()`** | ❌ | ❌ Not surfaced | **New — Watermarks view** |
| **`watermark_status()`** | ❌ | ❌ Not surfaced | **New — Watermarks view** |

---

### P1 — Diagnostics Signal Breakdown

**Problem:** The Diagnostics view (view `5`) shows the `recommend_refresh_mode()`
table—current mode, recommended mode, confidence, and reason—but the rich
`signals` JSONB column is discarded. This column contains per-signal scores
and weights that are critical for understanding *why* a recommendation was made.

**Solution:** When a row is selected in the Diagnostics view, render a signal
breakdown panel below the table with horizontal bar charts.

#### Poller Change

Add `signals` column to the `poll_diagnostics()` query:

```sql
SELECT pgt_schema::text, pgt_name::text, current_mode::text,
       recommended_mode::text, confidence::text, reason::text,
       signals::text   -- NEW
FROM pgtrickle.recommend_refresh_mode(NULL)
```

Parse signals JSON in Rust: `serde_json::from_str::<Value>()`.

#### State Change

```rust
pub struct DiagRecommendation {
    // ... existing fields ...
    pub signals: Option<serde_json::Value>,  // NEW
}
```

#### UI — Signal Bar Chart

When a diagnostic row is selected, render below the table:

```
┌─ Signal Breakdown: daily_revenue ──────────────────────────────────────┐
│                                                                         │
│  change_ratio_current  ████████████████████░░░░░  0.82  (wt: 0.25)     │
│  change_ratio_avg      ██████████████████████████  0.95  (wt: 0.30)     │
│  empirical_timing      █████████████████░░░░░░░░  0.71  (wt: 0.35)     │
│  query_complexity      ████████░░░░░░░░░░░░░░░░░  0.35  (wt: 0.10)     │
│  target_size           ██████████████░░░░░░░░░░░  0.58  (wt: 0.10)     │
│  index_coverage        ██████████████████████████  1.00  (wt: 0.05)     │
│  latency_variance      █████████░░░░░░░░░░░░░░░░  0.40  (wt: 0.05)     │
│                                                                         │
│  Composite: 0.79 → FULL recommended (above +0.15 threshold)             │
└─────────────────────────────────────────────────────────────────────────┘
```

Bars rendered with filled/empty block characters (`█`/`░`). Width scales
to available terminal width. **Height guard:** The signal panel is only
shown when terminal height ≥ 30; on shorter terminals, select a row and
press `Enter` to see signals in a centered overlay instead.

#### Tests Required

1. **Unit — signal parsing:** `test_signals_json_parse_valid` (7 signals → sorted Vec), `test_signals_json_parse_empty` (empty object → empty vec), `test_signals_json_parse_invalid` (garbage → None)
2. **Unit — bar width:** `test_signal_bar_width_at_80_cols` (score 1.0 fills available width), `test_signal_bar_width_zero` (score 0.0 → zero blocks)
3. **Snapshot:** `test_diagnostics_signal_breakdown_80x40` (full bar chart visible), `test_diagnostics_no_signals_80x24` (no signal panel when signals=None)
4. **Contract:** Stub `recommend_refresh_mode()` already exists — verify `signals` column is in result set
5. **Fixture:** `diag_with_signals()` builder in `test_fixtures.rs`

---

### P1 — Error Diagnosis Panel

**Problem:** When a stream table is in ERROR status, the Detail view shows
`last_error_message` but provides no historical error context, error
classification, or remediation guidance. The SQL function
`pgtrickle.diagnose_errors(name)` returns exactly this — last 5 errors,
each classified by type with a remediation hint.

**Solution:** In the Detail view, when the selected ST has
`consecutive_errors > 0` or `status = 'ERROR'`, fetch and display
`diagnose_errors()` inline.

> **Detail view layout priority:** With sources, explain mode, error
> diagnosis, and refresh history all added to the Detail view, height-aware
> rendering is essential. Rendering priority (top to bottom):
> 1. **Always:** Properties pane (name, status, mode, effective mode, schedule, tier)
> 2. **Always:** Sources section (if data available; collapses to 2 rows if >4 sources)
> 3. **Height ≥ 35:** Recent Refreshes (rich history with +N/-M)
> 4. **Height ≥ 40 + error state:** Error Diagnosis panel
> 5. **Height ≥ 45:** Efficiency sparkline (existing)

#### Design

Fetch on-demand (not every poll) when navigating to Detail for an
error-state table. Cache results in `AppState`.

```rust
pub struct DiagnosedError {
    pub event_time: String,
    pub error_type: String,     // user, schema, correctness, performance, infrastructure
    pub error_message: String,
    pub remediation: String,
}
```

#### UI Mockup

```
┌─ Error Diagnosis ─────────────────────────────────────────────────────┐
│                                                                       │
│  09:37:15  [schema]  upstream table schema changed                    │
│           → Reinitialize; check pgt_dependencies                      │
│                                                                       │
│  09:22:10  [schema]  upstream table schema changed                    │
│           → Reinitialize; check pgt_dependencies                      │
│                                                                       │
│  08:15:00  [user]    function max(jsonb) does not exist               │
│           → Check query; run validate_query()                         │
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘
```

Error types are color-coded: `user`=cyan, `schema`=yellow, `correctness`=red,
`performance`=magenta, `infrastructure`=red+bold.

The remediation line uses `theme.dim` and shows actionable next steps.

#### Tests Required

1. **Unit — color mapping:** `test_error_type_colors` (each of 5 types → correct color), `test_error_type_unknown_default` (unknown → dim)
2. **Unit — conditional display:** `test_diagnosis_hidden_for_active` (panel not rendered when consecutive_errors=0 and status≠ERROR)
3. **Snapshot:** `test_detail_error_with_diagnosis_80x40` (error table with diagnosed_errors populated), `test_detail_active_no_diagnosis_80x40` (no diagnosis panel)
4. **Contract:** New stub `diagnose_errors(text)` → `test_poll_diagnose_errors_executes`
5. **Degradation:** `test_poll_diagnose_errors_graceful_missing` — returns empty when function absent
6. **Fixture:** `diagnosed_error()` builder, `sample_state_full()` includes errors for error table

---

### P1 — Source Gating Actions

**Problem:** The Watermarks view shows watermark groups but does not display
source gate status, and operators cannot gate/ungate sources from the TUI.
The SQL functions `source_gates()`, `bootstrap_gate_status()`,
`gate_source()`, and `ungate_source()` exist but are not surfaced.

**Solution:** Extend the Watermarks view with a source gates tab and add
gate/ungate as write actions.

#### Poller Addition

Add `poll_source_gates()` to `poller.rs`:

```sql
SELECT source_table::text, schema_name::text, gated,
       gated_at::text, ungated_at::text, gated_by::text,
       gate_duration::text, affected_stream_tables::text
FROM pgtrickle.bootstrap_gate_status()
ORDER BY gated DESC, source_table
```

Also add `poll_watermark_status()` for alignment info:

```sql
SELECT group_name::text, min_watermark::text, max_watermark::text,
       lag_secs, aligned, sources_with_watermark, sources_total
FROM pgtrickle.watermark_status()
```

#### State Addition

```rust
pub struct SourceGate {
    pub source_table: String,
    pub schema_name: String,
    pub gated: bool,
    pub gated_at: Option<String>,
    pub gate_duration: Option<String>,
    pub affected_stream_tables: Option<String>,
}

pub struct WatermarkAlignment {
    pub group_name: String,
    pub lag_secs: Option<f64>,
    pub aligned: bool,
    pub sources_with_watermark: i32,
    pub sources_total: i32,
}
```

#### UI — Watermarks View Enhancement

Add a `Tab` key to cycle between two sub-views (matching the original
PLAN_TUI.md F15 design). A tab indicator in the view title shows the
current sub-view: `Watermarks [▁Groups▁] [Gates]` with the active tab
highlighted. First press of `g` on tab 1 auto-switches to tab 2.

**Tab 1: Watermarks & Alignment** (existing + `watermark_status()` data)

```
  Watermark Groups                      Alignment
  GROUP         MEMBERS  MIN_WM   MAX_WM   LAG     ALIGNED
  order_pipe    2        09:40    09:40    0s      ✓ yes
  event_pipe    2        09:30    09:38    480s    ✗ no (⚠)
```

**Tab 2: Source Gates**

```
  SOURCE           GATED  DURATION   AFFECTED STs
  public.events    ✓ yes  2h 15m     user_activity, daily_metrics
  public.orders    ✗ no   —          order_totals, big_customers
```

#### Actions

Add to `ActionRequest` enum:
```rust
GateSource(String),     // pgtrickle.gate_source($1)
UngateSource(String),   // pgtrickle.ungate_source($1)
```

Keybindings in Watermarks view (tab 2):
- `g` — Toggle gate/ungate for selected source
  - **Gate** (currently ungated → gated): requires `y/n` confirmation
    (gating blocks downstream refreshes)
  - **Ungate** (currently gated → ungated): executes immediately, no
    confirmation (safe — just resumes normal operation)

#### Tests Required

1. **Unit — key dispatch:** `test_key_g_in_watermarks_gates_tab` (dispatches GateSource for ungated / UngateSource for gated), `test_tab_in_watermarks_cycles_subtab`
2. **Unit — action confirmation:** `test_action_gate_requires_confirmation`, `test_action_ungate_no_confirmation`
3. **Snapshot:** `test_watermarks_gates_tab_100x30` (gate table), `test_watermarks_alignment_tab_100x30` (alignment lag + ✓/✗)
4. **Contract:** New stubs `bootstrap_gate_status()`, `watermark_status()`, `gate_source(text)`, `ungate_source(text)` → 4 contract tests
5. **Degradation:** `test_poll_source_gates_graceful_missing`, `test_render_watermarks_without_gates`
6. **Fixture:** `source_gate()`, `watermark_alignment()` builders

---

### P2 — Dedup Stats View

**Problem:** `pgtrickle.dedup_stats()` returns server-wide MERGE deduplication
counters (total diff refreshes, dedup needed count, ratio percentage). This
is valuable for diagnosing whether pre-MERGE compaction would help, but it's
not visible anywhere in the TUI.

**Solution:** Add a dedup stats summary to the CDC Health view (view `6`),
since deduplication is closely related to change buffer behavior.

#### Poller Addition

```sql
SELECT total_diff_refreshes, dedup_needed, dedup_ratio_pct
FROM pgtrickle.dedup_stats()
```

#### State Addition

```rust
pub struct DedupStats {
    pub total_diff_refreshes: i64,
    pub dedup_needed: i64,
    pub dedup_ratio_pct: f64,
}
```

#### UI — CDC View Enhancement

Add a summary block at the top of the CDC view:

```
┌─ MERGE Dedup Stats (since server start) ──────────────────────────────┐
│  Diff refreshes: 1,234   Dedup needed: 87 (7.1%)                      │
│  ████████░░░░░░░░░░░░░░░░ 7.1% dedup ratio                           │
│  Threshold: ≥10% → investigate two-pass MERGE                         │
└───────────────────────────────────────────────────────────────────────┘
```

Bar color: green (<5%), yellow (5-10%), red (≥10%).

**Height guard:** The dedup stats block is only rendered when the CDC view
has ≥30 rows of vertical space. On shorter terminals, the dedup data is
accessible via the `?` help overlay which shows a condensed summary.

#### Tests Required

1. **Unit — threshold colors:** `test_dedup_ratio_color_green` (<5%), `test_dedup_ratio_color_yellow` (5–10%), `test_dedup_ratio_color_red` (≥10%)
2. **Unit — issue detection:** `test_dedup_ratio_above_threshold_creates_issue` (detect_issues() adds warning when ≥10%)
3. **Snapshot:** `test_cdc_view_with_dedup_100x30` (stats block visible), `test_cdc_view_dedup_warning_100x30` (red threshold), `test_cdc_view_no_dedup_100x24` (no dedup block when None)
4. **Contract:** New stub `dedup_stats()` → `test_poll_dedup_stats_executes`
5. **Degradation:** `test_poll_dedup_stats_graceful_missing`, `test_render_cdc_without_dedup`
6. **Fixture:** `dedup_stats()` builder

---

### P2 — Shared Buffer Stats in CDC View

**Problem:** `pgtrickle.shared_buffer_stats()` shows per-source-table buffer
details — consumer count, columns tracked, safe frontier LSN, buffer rows,
and whether the buffer is partitioned. This is richer than `change_buffer_sizes()`
(which the TUI already polls) and provides shared-buffer–specific insight.

**Solution:** Add a "Buffer Detail" expandable panel in the CDC view. When
a CDC buffer row is selected, show the `shared_buffer_stats()` data for
that source.

#### Poller Addition

```sql
SELECT source_oid, source_table::text, consumer_count,
       consumers::text, columns_tracked,
       safe_frontier_lsn::text, buffer_rows, is_partitioned
FROM pgtrickle.shared_buffer_stats()
ORDER BY buffer_rows DESC
```

#### State Addition

```rust
pub struct SharedBufferInfo {
    pub source_table: String,
    pub consumer_count: i32,
    pub consumers: String,
    pub columns_tracked: i32,
    pub safe_frontier_lsn: Option<String>,
    pub buffer_rows: i64,
    pub is_partitioned: bool,
}
```

#### UI — CDC View Split

When a buffer row is selected and terminal height ≥ 30:

```
  Change Buffers
  SOURCE         BUFFER         ROWS    SIZE      MODE
▸ orders         changes_16384  42      128 kB    trigger
  events         changes_16387  1,247   4.2 MB    trigger ⚠

┌─ Buffer Detail: public.orders ──────────────────────────────────────┐
│  Consumers: 3 (order_totals, big_customers, exec_dashboard)         │
│  Columns tracked: 5    Partitioned: no                              │
│  Safe frontier LSN: 0/1A2B3C4D                                      │
│  Buffer rows: 42                                                    │
└─────────────────────────────────────────────────────────────────────┘
```

#### Tests Required

1. **Snapshot:** `test_cdc_buffer_detail_selected_100x30` (consumers, columns, LSN visible), `test_cdc_buffer_detail_no_selection_100x30` (panel hidden when no row selected), `test_cdc_buffer_detail_short_terminal_100x20` (panel hidden when height < 30)
2. **Contract:** New stub `shared_buffer_stats()` → `test_poll_shared_buffer_stats_executes`
3. **Degradation:** `test_poll_shared_buffers_graceful_missing`
4. **Fixture:** `shared_buffer()` builder

---

### P2 — Explain Refresh Mode in Detail

**Problem:** `pgtrickle.explain_refresh_mode(name)` returns the configured
mode vs the effective mode actually used, plus a human-readable downgrade
reason. This is useful for understanding why an AUTO table chose FULL
instead of DIFFERENTIAL, but it's not shown anywhere.

**Solution:** Add a line to the Detail view Properties pane.

#### Design

Fetch on-demand when entering Detail view (per selected table).
Show a one-liner below the existing Mode line:

```
  Mode:      AUTO
  Effective: FULL (↓ downgraded)
   → High change ratio (82%) exceeds adaptive threshold
```

Green if configured == effective. Yellow with `↓ downgraded` if they differ.

#### Tests Required

1. **Unit — style logic:** `test_mode_same_is_green` (configured == effective → green), `test_mode_downgraded_is_yellow` (differ → yellow + "↓ downgraded")
2. **Snapshot:** `test_detail_downgraded_mode_80x40`, `test_detail_same_mode_80x40`
3. **Contract:** New stub `explain_refresh_mode(text)` → `test_poll_explain_refresh_mode_executes`
4. **Degradation:** `test_poll_explain_mode_graceful_missing`
5. **Fixture:** `explain_mode()` builder

---

### P2 — Source Table Detail

**Problem:** `pgtrickle.list_sources(name)` returns the source tables for a
stream table with CDC mode, source type, and columns used. The Detail view
currently does not show source tables at all.

**Solution:** Add a "Sources" section to the Detail view.

#### Poller Addition

Fetch on-demand (when Detail view is entered for a specific table):

```sql
SELECT source_table::text, source_oid, source_type::text,
       cdc_mode::text, columns_used::text
FROM pgtrickle.list_sources($1)
ORDER BY source_table
```

#### UI — Detail View Addition

```
┌─ Sources ──────────────────────────────────────────────────────────────┐
│  SOURCE              TYPE           CDC MODE   COLUMNS                 │
│  public.orders       table          trigger    customer_id, amount     │
│  public.products     table          trigger    id, name, price         │
└───────────────────────────────────────────────────────────────────────┘
```

#### Tests Required

1. **Snapshot:** `test_detail_sources_section_80x40` (2 sources with columns), `test_detail_no_sources_80x40` (no sources section when empty)
2. **Contract:** New stub `list_sources(text)` → `test_poll_list_sources_executes`
3. **Degradation:** `test_poll_list_sources_graceful_missing`, `test_render_detail_without_sources`
4. **Fixture:** `source_info()` builder

---

### P2 — Diamond Groups & SCC Status

**Problem:** `pgtrickle.diamond_groups()` and `pgtrickle.pgt_scc_status()`
return structural DAG information — diamond consistency groups and cyclic
SCC groups — but neither is surfaced in the TUI.

**Solution:** Enrich the Graph view and Detail view with diamond/SCC
information.

#### Poller Addition

Add two new poll functions:

```sql
-- Diamond groups
SELECT group_id, member_name::text, member_schema::text,
       is_convergence, epoch, schedule_policy::text
FROM pgtrickle.diamond_groups()
ORDER BY group_id, member_name

-- SCC status
SELECT scc_id, member_count, members::text,
       last_iterations, last_converged_at::text
FROM pgtrickle.pgt_scc_status()
ORDER BY scc_id
```

#### UI — Graph View Enhancement

In the Graph view, annotate nodes with diamond (◆) and cycle (○) markers:
- Diamond group members share a group badge: `◆1`, `◆2`
- SCC members show `○` with iteration count

Selected node metadata shows:
```
  Selected: order_totals  ◆ Diamond group 1 (epoch: 42, policy: fastest)
```
or:
```
  Selected: reach_a  ○ SCC group 1 (2 members, 3 iterations, converged 5m ago)
```

#### UI — Detail View Enhancement

If the selected ST is in a diamond group or SCC, add a badge in Properties:
```
  Diamond:  ◆ Group 1 — atomic (epoch 42, fastest policy)
  SCC:      ○ Group 1 — 2 members, last convergence: 3 iterations
```

#### Tests Required

1. **Unit — lookup:** `test_diamond_group_members_for_table` (returns correct group), `test_scc_group_members_for_table` (returns empty when not in SCC)
2. **Snapshot — Graph:** `test_graph_diamond_markers_80x24` (◆ badges on nodes), `test_graph_no_diamond_80x24` (no markers when empty)
3. **Snapshot — Detail:** `test_detail_diamond_badge_80x40` (diamond badge in Properties), `test_detail_scc_badge_80x40` (SCC badge)
4. **Contract:** New stubs `diamond_groups()`, `pgt_scc_status()` → 2 contract tests
5. **Degradation:** `test_poll_diamond_groups_graceful_missing`
6. **Fixture:** `diamond_group()`, `scc_group()` builders

---

### P2 — Quick Health in Header Bar

**Problem:** `pgtrickle.quick_health` returns a single-row summary with
`total_stream_tables`, `error_tables`, `stale_tables`, `scheduler_running`,
and overall `status` (OK/WARNING/CRITICAL). The TUI header currently
computes these client-side from polled data, but doesn't have the
authoritative `scheduler_running` check.

**Solution:** Poll `quick_health` and use `scheduler_running` to add a
scheduler indicator to the header bar.

#### Header Enhancement

```
pg_trickle  Dashboard  ● connected  ⚙ scheduler  ⏱ 2s  polled 1s ago
```

The `⚙ scheduler` indicator:
- Green `⚙ scheduler` when `scheduler_running = true`
- Red `✗ scheduler stopped` when `scheduler_running = false`
- Red + issue auto-detected: add a CRITICAL issue to the Issues view

#### Tests Required

1. **Unit — issue detection:** `test_scheduler_stopped_creates_issue` (detect_issues() adds CRITICAL when scheduler_running=false)
2. **Snapshot:** `test_dashboard_scheduler_running_80x24` (green indicator), `test_dashboard_scheduler_stopped_80x24` (red indicator)
3. **Contract:** New stub/view `quick_health` → `test_poll_quick_health_executes`
4. **Degradation:** `test_poll_quick_health_graceful_missing` (header omits indicator)
5. **Fixture:** `quick_health()` builder, `sample_state_full()` includes it

---

### P2 — CDC Health Deep Dive

**Problem:** The CDC view currently shows `change_buffer_sizes()` and
`trigger_inventory()` but not the richer `check_cdc_health()` function.
This provides WAL slot status, lag bytes, confirmed LSN, and alert messages
per source that are critical for diagnosing WAL-mode CDC issues.

**Solution:** Add WAL slot health section to CDC view. (The separate
`slot_health()` function is not needed — `check_cdc_health()` already
includes slot information per source.)

Note: With dedup stats, shared buffer detail, CDC health, and triggers all
in the CDC view, layout is height-sensitive. See the rendering priority:
1. **Always shown:** Change buffers table (primary data)
2. **Height ≥ 30:** CDC Source Health section appears below buffers
3. **Height ≥ 35:** Dedup stats block appears at top
4. **Height ≥ 40 + row selected:** Buffer detail panel appears below buffers
5. **Always shown (bottom):** Trigger inventory (collapsed to 3 rows if needed)

#### Poller Addition

```sql
SELECT source_table::text, cdc_mode::text, slot_name::text,
       lag_bytes, confirmed_lsn::text, alert::text
FROM pgtrickle.check_cdc_health()
ORDER BY COALESCE(lag_bytes, 0) DESC
```

#### UI — CDC View Enhancement

Add between the buffer table and trigger inventory:

```
┌─ CDC Source Health ─────────────────────────────────────────────────────┐
│  SOURCE         MODE         SLOT                   LAG         ALERT   │
│  orders         trigger      —                      —           —       │
│  events         wal          pg_trickle_slot_16390   512 kB     —       │
│  logs           transitioning pg_trickle_slot_16392  1.2 MB     ⚠ lag   │
└─────────────────────────────────────────────────────────────────────────┘
```

Alert column rendered in red when non-null.

#### Tests Required

1. **Snapshot:** `test_cdc_health_section_100x30` (slot names, lag, alert column visible), `test_cdc_health_no_wal_100x24` (section still renders with trigger-only sources)
2. **Contract:** New stub `check_cdc_health()` → `test_poll_cdc_health_executes`
3. **Degradation:** `test_poll_cdc_health_graceful_missing`
4. **Fixture:** `cdc_health()` builder with trigger/wal/transitioning variants

---

### P2 — Refresh History Detail

**Problem:** The Detail view shows recent refreshes from `refresh_timeline()`,
but this function lacks `rows_inserted`, `rows_deleted`, and `delta_row_count`
per entry. The richer `get_refresh_history(name, N)` function provides these.

**Solution:** In the Detail view, use `get_refresh_history()` instead of
filtering `refresh_timeline()` for the per-table recent refresh list.

#### Poller Change

Fetch on-demand when Detail view is entered:

```sql
SELECT action::text, status::text, rows_inserted, rows_deleted,
       delta_row_count, duration_ms, was_full_fallback,
       start_time::text, error_message::text
FROM pgtrickle.get_refresh_history($1, 10)
ORDER BY start_time DESC
```

#### UI Enhancement

```
┌─ Recent Refreshes ───────────────────────────────────────────────────┐
│  09:42:18  DIFF  38ms  +42 -3  (42 Δ)  ✓                            │
│  09:37:16  DIFF  41ms  +18 -0  (18 Δ)  ✓                            │
│  09:32:14  DIFF  22ms   +3 -0   (3 Δ)  ✓                            │
│  09:22:10  FULL  470ms  +1000   —       ✓ (fallback)                 │
└──────────────────────────────────────────────────────────────────────┘
```

The `+N -M` shows rows_inserted/deleted. `(N Δ)` shows delta_row_count.
Fallback indicator when `was_full_fallback = true`.

#### Tests Required

1. **Unit — formatting:** `test_refresh_history_diff_format` ("+42 -3 (42 Δ)" output), `test_refresh_history_full_fallback` ("(fallback)" indicator)
2. **Snapshot:** `test_detail_rich_history_80x40` (rows_inserted/deleted visible), `test_detail_empty_history_80x40`
3. **Contract:** New stub `get_refresh_history(text, int)` → `test_poll_refresh_history_executes`
4. **Degradation:** `test_poll_refresh_history_graceful_missing`
5. **Fixture:** `refresh_history()` builder with diff/full/error variants

---

### P2 — Repair Action

**Problem:** `pgtrickle.repair_stream_table(name)` reinstalls missing CDC
triggers and validates catalog integrity. This is needed after PITR restores
or when `trigger_inventory()` shows missing triggers. Currently requires
psql or CLI.

**Solution:** Add `repair` as a write action available from:
1. Command palette: `:repair <name>`
2. Detail view: keybinding (when health check indicates missing triggers)
3. Fuse view: for tables with infrastructure errors

#### ActionRequest Addition

```rust
RepairTable(String),  // pgtrickle.repair_stream_table($1)
```

#### Tests Required

1. **Unit — key dispatch:** `test_repair_command_parse` (`:repair <name>` parses correctly)
2. **Unit — confirmation:** `test_action_repair_requires_confirmation`
3. **Contract:** New stub `repair_stream_table(text)` → `test_action_repair_executes`
4. **Integration:** Toast shows success/failure message after repair

---

### P2 — Export DDL Inline

**Problem:** `pgtrickle.export_definition(name)` generates reproducible DDL
for a stream table. The CLI has `pgtrickle export <name>` but the TUI Detail
view doesn't offer it.

**Solution:** Add `e` keybinding in Detail view that fetches the DDL and
shows it in a scrollable overlay (like the help overlay).

#### Design

Fetch on-demand: `SELECT pgtrickle.export_definition($1)`.
Display in a centered overlay with syntax highlighting (same as
delta SQL highlighting). `Esc` dismisses the overlay. `y` copies DDL to
a tempfile (`/tmp/pgtrickle_ddl_<name>.sql`) and shows the path as a toast.

> Note: Direct clipboard access via `crossterm` is unreliable across
> terminal emulators. The tempfile approach is more portable.

#### Tests Required

1. **Unit — key dispatch:** `test_key_e_in_detail_triggers_ddl_fetch` (e key dispatches FetchDdl action)
2. **Snapshot:** `test_detail_ddl_overlay_80x30` (DDL text visible in overlay)
3. **Contract:** Stub `export_definition(text)` already exists — verify TUI fetch path against it

---

### P2 — Query Validation

**Problem:** `pgtrickle.validate_query(query)` validates a defining query
through the DVM pipeline and returns detected constructs, warnings, and the
resolved refresh mode — without creating a stream table. Useful for
pre-flight checks before `create_stream_table`.

**Solution:** Add a `:validate` command to the command palette. The user
pastes a query, and the TUI displays the validation results in an overlay.

Command: `:validate SELECT customer_id, COUNT(*) FROM orders GROUP BY customer_id`

> **UX note:** Multi-line queries can be pasted into the single-line palette
> as-is — newlines are normalized to spaces before sending to
> `validate_query()`. For very long queries, the palette input scrolls
> horizontally. The overlay shows the parsed query for confirmation.

#### UI — Validation Results Overlay

```
┌─ Query Validation ───────────────────────────────────────────────────┐
│                                                                       │
│  Resolved mode:  DIFFERENTIAL                                         │
│                                                                       │
│  CHECK                      RESULT                         SEV        │
│  ─────────────────────────  ───────────────────────────     ────       │
│  resolved_refresh_mode      DIFFERENTIAL                   INFO       │
│  aggregate                  SUM(ALGEBRAIC_INVERTIBLE)       INFO       │
│  aggregate                  COUNT(ALGEBRAIC_INVERTIBLE)     INFO       │
│  needs_pgt_count            true                           INFO       │
│  volatility                 immutable                       INFO       │
│                                                                       │
│  ✓ Query is valid for DIFFERENTIAL mode                                │
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘
```

Warnings in yellow, errors in red.

#### Tests Required

1. **Unit — command parsing:** `test_validate_command_parse` (`:validate SELECT ...` extracted correctly), `test_validate_empty_query_rejected`
2. **Snapshot:** `test_validate_overlay_80x30` (checks table, resolved mode), `test_validate_overlay_with_warnings_80x30` (yellow warning rows)
3. **Contract:** New stub `validate_query(text)` → `test_cmd_validate_executes`
4. **Degradation:** `test_validate_graceful_missing` (shows error toast when function absent)

---

### P3 — Auxiliary Columns View

**Problem:** `pgtrickle.list_auxiliary_columns(name)` exposes the hidden
`__pgt_*` columns with explanations. This is useful for debugging delta
computation but is not surfaced.

**Solution:** Add an "Internals" tab to the Delta Inspector view.

#### UI — Delta Inspector Enhancement

When on the Delta Inspector view, `Tab` cycles between:
1. Delta SQL (existing, enhanced with inline fetch)
2. Auxiliary Columns (new)

```
┌─ Auxiliary Columns: order_totals ────────────────────────────────────┐
│                                                                       │
│  COLUMN                   TYPE             PURPOSE                    │
│  ──────────────────────── ──────────────── ────────────────────────── │
│  __pgt_row_id             bigint           Row identity hash (PK)     │
│  __pgt_count              bigint           Group multiplicity (SUM)   │
│  __pgt_aux_sum_total      numeric          Running SUM for AVG        │
│  __pgt_aux_count_total    bigint           Running COUNT for AVG      │
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘
```

#### Tests Required

1. **Unit — tab cycling:** `test_tab_in_delta_inspector_cycles` (Tab key cycles SQL → Aux → SQL)
2. **Snapshot:** `test_delta_inspector_aux_tab_80x24` (column names, types, purposes), `test_delta_inspector_aux_empty_80x24` ("No auxiliary columns" placeholder)
3. **Contract:** New stub `list_auxiliary_columns(text)` → `test_poll_auxiliary_columns_executes`
4. **Degradation:** `test_poll_auxiliary_columns_graceful_missing`
5. **Fixture:** `aux_column()` builder

---

## Implementation Phases

### Phase T9 — Write Actions & Reconnect (Day 1–2)

**Goal:** Transform the TUI from read-only to operational. Fix the poller
connection resilience gap.

| Item | Description | Effort |
|------|-------------|--------|
| T9a | `ActionRequest` enum + action channel (UI → poller) | 1h |
| T9b | Action execution in poller task (refresh, pause, resume, fuse reset) | 2h |
| T9c | Result feedback channel (poller → UI) + toast rendering | 2h |
| T9d | Confirmation dialog (single-line y/n prompt in footer) | 1h |
| T9e | Keybindings: `r`, `R`, `p`, `P`, `A` wired to actions | 1h |
| T9f | Poller auto-reconnect with exponential backoff | 2h |
| T9g | `all_polls_failed()` detection + reconnect state indicators | 1h |
| T9h | Tests: action channel unit tests, reconnect backoff tests | 1h |

**Exit:** `r` refreshes the selected table, result shown as toast. `R`
with confirmation refreshes all. `p`/`P` pause/resume. Poller reconnects
after connection loss with backoff.

### Phase T10 — Scrollable Views & Poll Config (Day 2–3)

**Goal:** Fix the scroll/viewport problem and add poll interval configuration.

| Item | Description | Effort |
|------|-------------|--------|
| T10a | `ViewStates` struct with per-view `TableState` instances | 1h |
| T10b | Migrate Dashboard to `StatefulWidget<TableState>` | 2h |
| T10c | Migrate remaining 9 table-based views | 3h |
| T10d | Graph + Alert views: manual scroll offset | 1h |
| T10e | `Scrollbar` widget for views with overflow | 1h |
| T10f | `PgUp`/`PgDn`/`Ctrl+U`/`Ctrl+D` page navigation | 30m |
| T10g | `--interval` CLI flag, header display, poller integration | 1h |
| T10h | Tests: snapshot tests for scrolled views | 1h |

**Exit:** All table views scroll properly with large datasets. Scrollbar
visible when content overflows. `--interval 5` sets 5-second polling.

### Phase T11 — Command Palette & TLS (Day 3–4)

**Goal:** Implement the command palette and production-grade TLS connections.

| Item | Description | Effort |
|------|-------------|--------|
| T11a | `CommandPalette` struct, input handling, overlay rendering | 2h |
| T11b | Command grammar parser (refresh, pause, resume, alter, fuse reset, explain, export, quit) | 2h |
| T11c | Fuzzy suggestion matching with ST name completion | 1h |
| T11d | Command execution → ActionRequest/view-switch dispatch | 1h |
| T11e | Add `tokio-postgres-rustls`, `rustls`, `webpki-roots` deps | 30m |
| T11f | `--sslmode` flag, `SslMode` enum, connect() TLS logic | 2h |
| T11g | TLS fallback (prefer mode) + require mode | 1h |
| T11h | Tests: command parsing tests, TLS connection tests (if testcontainers supports TLS) | 1h |

**Exit:** `:` opens command palette. Tab-completes ST names. `refresh`,
`pause`, `resume`, `alter`, `fuse reset` execute from palette.
`--sslmode require` connects via TLS.

### Phase T12 — Sort, Filter, Delta, Toast (Day 4–5)

**Goal:** Polish the information display and interaction feedback.

| Item | Description | Effort |
|------|-------------|--------|
| T12a | `SortMode` enum, `s`/`S` key handling, sort indicator in header | 1h |
| T12b | Sort comparator for each mode (name, duration, refreshes, staleness, last refresh) | 1h |
| T12c | Inline delta SQL fetch: `FetchDeltaSql` action, cache, rendering | 2h |
| T12d | Simple SQL keyword highlighting (regex-based coloring) | 1h |
| T12e | Cross-view filter: per-view `matches_filter()` predicates | 1h |
| T12f | Filter persistence: keep filter on view switch, clear with Esc | 30m |
| T12g | Toast struct, rendering in footer, auto-expire logic | 1h |
| T12h | Wire toasts to all action results (refresh, pause, reconnect, export) | 30m |

**Exit:** `s` cycles sort order on Dashboard. Delta Inspector shows actual
SQL. Filter applies across all list views. Actions produce visible toast
feedback.

### Phase T12b — SQL API Surface: Diagnostics & Errors (Day 5)

**Goal:** Surface the diagnostic and error-classification SQL functions.

| Item | Description | Effort |
|------|-------------|--------|
| T12b-a | Poll `recommend_refresh_mode()` with `signals` column | 30m |
| T12b-b | Signal JSONB parsing + bar chart rendering in Diagnostics view | 2h |
| T12b-c | `diagnose_errors()` on-demand fetch + state cache | 1h |
| T12b-d | Error Diagnosis panel rendering in Detail view | 1h |
| T12b-e | `explain_refresh_mode()` on-demand fetch in Detail view | 1h |
| T12b-f | `list_sources()` on-demand fetch + Sources section in Detail view | 1h |
| T12b-g | `get_refresh_history()` with rows_inserted/deleted in Detail | 1h |
| T12b-h | **Fixtures:** `diag_with_signals()`, `diagnosed_error()`, `explain_mode()`, `source_info()`, `refresh_history()` builders | 1h |
| T12b-i | **Fixtures:** `sample_state_full()` that includes all T12b data | 30m |
| T12b-j | **Unit tests:** Signal JSON parsing (valid/empty/invalid), bar width calc, error type color mapping | 1h |
| T12b-k | **Unit tests:** Mode downgrade style logic, refresh history formatting (+N -M) | 30m |
| T12b-l | **Contract stubs:** Add `diagnose_errors`, `explain_refresh_mode`, `list_sources`, `get_refresh_history` to `test_db.rs` | 1h |
| T12b-m | **Contract tests:** 4 new `test_poll_*_executes` tests against stubs | 30m |
| T12b-n | **Degradation tests:** 4 `test_poll_*_graceful_missing` tests (function-not-found → empty state) | 30m |
| T12b-o | **Snapshot tests:** Diagnostics w/ signal bar chart, Detail w/ error diagnosis, Detail w/ sources, Detail w/ mode downgrade, Detail w/ rich history (5 tests, 2 sizes each) | 2h |

**Exit:** Diagnostics view shows signal bar charts for selected table.
Detail view shows error classification with remediation hints, effective
vs configured mode, source tables, and richer refresh history.
All unit, snapshot, contract, and degradation tests pass.

### Phase T13 — Parallel Polling & Minor Features (Day 5–6)

**Goal:** Performance improvement + quality-of-life additions.

| Item | Description | Effort |
|------|-------------|--------|
| T13a | Refactor `poll_*` functions to return `Option<Vec<T>>` | 2h |
| T13b | `tokio::join!` in `poll_all()` for concurrent execution | 1h |
| T13c | Verify sparkline data collection still works (was per-poll mutation) | 30m |
| T13d | Mouse support: `--mouse` flag, `EnableMouseCapture`, hit-testing | 2h |
| T13e | Scroll wheel handling | 30m |
| T13f | Click-to-select rows | 1h |
| T13g | Theme toggle: `Theme::light()`, `t` key, `--theme` flag | 1h |
| T13h | Export: `Ctrl+E`, JSON file write, toast with path | 1h |
| T13i | Bell: `--bell` flag, critical alert detection, rate limiting | 30m |
| T13j | Update help overlay with all new keybindings | 1h |

**Exit:** Poll time reduced on high-latency connections. Mouse-enabled with
`--mouse`. Light theme available. `Ctrl+E` exports view data. `--bell`
enables audible critical alerts.

### Phase T13b — SQL API Surface: CDC, DAG & Actions (Day 6–7)

**Goal:** Surface remaining SQL functions: CDC deep dive, diamond/SCC,
source gating, dedup stats, repair, and export DDL.

| Item | Description | Effort |
|------|-------------|--------|
| T13b-a | `poll_dedup_stats()` + dedup stats block in CDC view | 1h |
| T13b-b | `poll_shared_buffer_stats()` + buffer detail panel in CDC view | 1h |
| T13b-c | `poll_cdc_health()` via `check_cdc_health()` + WAL slot section in CDC view | 1h |
| T13b-d | `poll_diamond_groups()` + `poll_scc_status()` + markers in Graph/Detail | 2h |
| T13b-e | `poll_source_gates()` + `poll_watermark_status()` + Tab in Watermarks view | 2h |
| T13b-f | Gate/ungate action: `ActionRequest::GateSource` / `UngateSource` | 1h |
| T13b-g | `poll_quick_health()` + scheduler indicator in header | 1h |
| T13b-h | `RepairTable` action + wiring in command palette and Fuse view | 30m |
| T13b-i | Export DDL overlay: `e` key in Detail, fetch `export_definition()` | 1h |
| T13b-j | `:validate` command in palette + overlay rendering | 1h |
| T13b-k | `list_auxiliary_columns()` tab in Delta Inspector | 1h |
| T13b-l | **Fixtures:** `dedup_stats()`, `shared_buffer()`, `cdc_health()`, `source_gate()`, `watermark_alignment()`, `diamond_group()`, `scc_group()`, `quick_health()`, `aux_column()` builders | 1.5h |
| T13b-m | **Fixtures:** Extend `sample_state_full()` with all T13b data fields | 30m |
| T13b-n | **Unit tests:** Dedup ratio color thresholds (3 tests), scheduler-stopped issue detection, diamond/SCC lookup helpers, tab cycling in Watermarks/Delta Inspector, gate toggle dispatch, validate/repair command parsing | 2h |
| T13b-o | **Contract stubs:** Add `dedup_stats`, `shared_buffer_stats`, `check_cdc_health`, `diamond_groups`, `pgt_scc_status`, `bootstrap_gate_status`, `watermark_status`, `quick_health` view, `list_auxiliary_columns`, `validate_query`, `repair_stream_table`, `gate_source`, `ungate_source` to `test_db.rs` | 2h |
| T13b-p | **Contract tests:** 13 new `test_poll_*_executes` + `test_action_*_executes` tests | 1h |
| T13b-q | **Degradation tests:** 8 `test_poll_*_graceful_missing` tests + 3 `test_render_*_without_*` rendering fallback tests | 1h |
| T13b-r | **Snapshot tests:** CDC w/ dedup + buffer detail + WAL health, Graph w/ diamond markers, Detail w/ diamond/SCC badges, Watermarks gates tab + alignment tab, Dashboard w/ scheduler indicator (running + stopped), DDL overlay, validate overlay, Delta Inspector aux tab (12 snapshots) | 3h |
| T13b-s | **Action tests:** Gate requires confirmation, ungate no confirmation, repair requires confirmation, action success/error toast, gate toggle state logic | 1h |

**Exit:** CDC view shows dedup stats, shared buffer detail, and WAL slot
health. Graph/Detail show diamond and SCC badges. Watermarks view has
source gate tab with gate/ungate actions. Header shows scheduler status.
Repair available from palette. Export DDL from Detail. Validate from palette.
All unit, snapshot, contract, degradation, and action tests pass.

### Phase T14 — Testing, Polish & Documentation (Day 7–10)

**Goal:** Comprehensive test coverage, cross-cutting integration validation,
documentation updates, and CI green.

#### T14-A: Test Infrastructure Foundation (Day 7)

| Item | Description | Effort |
|------|-------------|--------|
| T14a-1 | Audit all new state structs have fixture builders in `test_fixtures.rs` | 30m |
| T14a-2 | Verify `sample_state_full()` includes data for every new field; add missing data | 30m |
| T14a-3 | Audit all new SQL stubs in `test_db.rs`; verify column names/types match `SQL_REFERENCE.md` exactly | 1h |
| T14a-4 | Run all contract tests; fix any stub signature drift | 30m |

#### T14-B: Cross-Cutting Snapshot Tests (Day 7–8)

These test interactions between features (features tested individually in
T12b/T13b; here we test them composed together).

| Item | Description | Effort |
|------|-------------|--------|
| T14b-1 | **CDC view fully loaded:** Dedup stats + buffer detail (selected) + CDC health section — all rendered in one 100×40 terminal | 1h |
| T14b-2 | **Detail view fully loaded:** Error diagnosis + sources + explain mode + rich history + export DDL overlay + diamond badge — all in one 100×60 terminal | 1h |
| T14b-3 | **Dashboard header states:** Connected w/ scheduler running, reconnecting, scheduler stopped, disconnected — 4 snapshots | 1h |
| T14b-4 | **Watermarks fully loaded:** Alignment tab + gates tab — Tab toggle test | 30m |
| T14b-5 | **Delta Inspector fully loaded:** Delta SQL tab + aux columns tab — Tab toggle test | 30m |
| T14b-6 | **Graph with rich annotations:** Diamond + SCC markers + error states + cascade staleness | 30m |
| T14b-7 | **Diagnostics with signal panel:** 80×24 (clipped) vs 120×40 (full) — responsive rendering | 30m |
| T14b-8 | **All views at narrow terminal:** 60×20 — verify nothing panics; graceful truncation | 1h |
| T14b-9 | **All views with empty state:** `empty_state()` — every view renders placeholder, no panics | 1h |
| T14b-10 | **Filtered views with new data:** Filter "orders" applied to CDC (dedup stats still visible), Watermarks (gates filtered), Detail (sources filtered) | 30m |

#### T14-C: Integration Tests — Actions & Lifecycle (Day 8–9)

These use `PgtStubDb` (Testcontainers) to test the full action lifecycle.

| Item | Description | Effort |
|------|-------------|--------|
| T14c-1 | **Write action round-trip:** Dispatch RefreshTable action → verify stub called → verify toast produced | 1h |
| T14c-2 | **Gate action round-trip:** Dispatch GateSource → verify y/n confirmation → verify stub `gate_source()` called | 1h |
| T14c-3 | **Ungate action:** Dispatch UngateSource → no confirmation → stub called | 30m |
| T14c-4 | **Repair action round-trip:** Dispatch RepairTable → confirmation → stub called → toast | 30m |
| T14c-5 | **Validate action:** Dispatch ValidateQuery → stub `validate_query()` called → results returned | 30m |
| T14c-6 | **Export DDL:** Dispatch FetchDdl → stub `export_definition()` called → DDL string returned | 30m |
| T14c-7 | **Reconnect scenario:** Kill DB connection → verify backoff sequence (1s→2s→4s) → verify state reflects reconnecting → verify recovery | 2h |
| T14c-8 | **Action during reconnect:** Send action while disconnected → verify error toast, no crash | 30m |
| T14c-9 | **Parallel poll:** Verify `tokio::join!` completes all 17+ queries successfully against stubs | 1h |

#### T14-D: Graceful Degradation Suite (Day 9)

Verify the TUI works correctly against older extension versions that lack
newer SQL functions. Each test installs only the base stubs (v0.13 functions)
and verifies the TUI handles missing functions gracefully.

| Item | Description | Effort |
|------|-------------|--------|
| T14d-1 | **Missing dedup_stats:** CDC view renders without dedup block; no error | 30m |
| T14d-2 | **Missing shared_buffer_stats:** CDC view renders without buffer detail; no error | 30m |
| T14d-3 | **Missing check_cdc_health:** CDC view shows only trigger inventory; no error | 30m |
| T14d-4 | **Missing diagnose_errors:** Detail view renders without diagnosis panel; no error | 30m |
| T14d-5 | **Missing diamond_groups / pgt_scc_status:** Graph and Detail render without badges; no error | 30m |
| T14d-6 | **Missing quick_health:** Header renders without scheduler indicator; no error | 30m |
| T14d-7 | **Missing bootstrap_gate_status:** Watermarks view shows only groups tab; no error | 30m |
| T14d-8 | **Missing list_auxiliary_columns:** Delta Inspector shows only SQL tab; no error | 30m |
| T14d-9 | **All functions missing:** Install _no_ new stubs, only base set. Verify TUI launches, polls, renders all views without panics. This is the "v0.13 compatibility" test. | 1h |

#### T14-E: Documentation & Polish (Day 9–10)

| Item | Description | Effort |
|------|-------------|--------|
| T14e-1 | Update `docs/TUI.md` with new keybindings, CLI flags, features, SQL functions surfaced | 1h |
| T14e-2 | Update help overlay context tips for all modified views (CDC, Diagnostics, Detail, Watermarks, Delta Inspector) | 1h |
| T14e-3 | Update CHANGELOG.md with all 30 features | 30m |
| T14e-4 | `just fmt && just lint` — zero warnings across entire TUI crate | 30m |
| T14e-5 | Update PLAN_TUI.md exit criteria (mark deferred items as completed) | 30m |
| T14e-6 | Run `insta review` and accept all new/changed snapshots | 30m |
| T14e-7 | Manual smoke test: connect to a real pg_trickle instance, verify all 30 features end-to-end | 1h |

#### T14-F: Coverage Audit (Day 10)

| Item | Description | Effort |
|------|-------------|--------|
| T14f-1 | Run `cargo tarpaulin` or `cargo llvm-cov` on TUI crate; measure per-module coverage | 30m |
| T14f-2 | Review uncovered lines in `state.rs` — target ≥85%; add missing tests | 1h |
| T14f-3 | Review uncovered lines in `app.rs` — target ≥75%; add missing key dispatch tests | 1h |
| T14f-4 | Review uncovered lines in `poller.rs` — target ≥65%; verify via contract test count | 30m |
| T14f-5 | Review uncovered view rendering paths; add boundary snapshots for any view <80% | 1h |
| T14f-6 | Final green CI run: `just lint && cargo test --workspace` | 30m |

**Exit:** All tests pass. Graceful degradation verified against base stubs.
No panics at any terminal size (60×20 through 200×60). Coverage targets met.
Docs current. CI green. `just lint` clean. No snapshot drift.

---

## Exit Criteria

### Feature Completeness

- [ ] **P0 — Write actions:** `r` refreshes selected, `R` refreshes all (with confirmation), `p`/`P` pause/resume, `A` resets fuse — all produce toast feedback
- [ ] **P0 — Reconnect:** Poller detects connection loss, reconnects with exponential backoff (1s→2s→4s→8s→15s cap), header reflects state
- [ ] **P1 — Command palette:** `:` opens palette, fuzzy-matches commands, tab-completes ST names, executes refresh/pause/resume/alter/fuse-reset
- [ ] **P1 — Scrollable views:** All 12 list-based views handle datasets larger than the visible area; scrollbar appears when needed; PgUp/PgDn work
- [ ] **P1 — Poll interval:** `--interval` flag accepted, value displayed in header, poller uses configured interval
- [ ] **P1 — TLS:** `--sslmode disable|prefer|require` works; `prefer` falls back to plaintext; `require` fails without TLS
- [ ] **P1 — Diagnostics signals:** Bar chart rendering of `recommend_refresh_mode().signals` JSONB for selected table
- [ ] **P1 — Error diagnosis:** `diagnose_errors()` displayed in Detail view with error classification and remediation hints
- [ ] **P1 — Source gating:** `bootstrap_gate_status()` displayed in Watermarks view; `gate_source()`/`ungate_source()` actions via `g` key
- [ ] **P2 — Sort:** `s` cycles sort mode on Dashboard; `S` reverses; column header shows ▲/▼ indicator
- [ ] **P2 — Delta SQL:** Delta Inspector fetches and displays actual SQL with keyword highlighting; scrollable
- [ ] **P2 — Filter:** `/` filter persists across view switches and applies to all list views
- [ ] **P2 — Toast:** Action results shown as auto-clearing footer message (3s); queue handles rapid actions
- [ ] **P2 — Parallel poll:** 17+ queries execute concurrently; measured improvement on 50ms+ RTT connections
- [ ] **P2 — Dedup stats:** `dedup_stats()` summary in CDC view with ratio bar and threshold indicator
- [ ] **P2 — Shared buffers:** `shared_buffer_stats()` detail panel in CDC view for selected buffer
- [ ] **P2 — Explain mode:** `explain_refresh_mode()` shown in Detail Properties (configured vs effective + downgrade reason)
- [ ] **P2 — Source tables:** `list_sources()` shown in Detail view with CDC mode and columns used
- [ ] **P2 — Diamond/SCC:** `diamond_groups()` and `pgt_scc_status()` badges in Graph and Detail views
- [ ] **P2 — Quick health:** `quick_health` polled; `scheduler_running` shown as indicator in header bar
- [ ] **P2 — CDC deep dive:** `check_cdc_health()` WAL slot section in CDC view with lag and alert columns
- [ ] **P2 — Refresh history:** `get_refresh_history()` used in Detail with rows_inserted/deleted/delta_row_count
- [ ] **P2 — Repair action:** `repair_stream_table()` available from command palette and Fuse view
- [ ] **P2 — Export DDL:** `export_definition()` shown in scrollable overlay from Detail view via `e` key
- [ ] **P2 — Query validation:** `:validate <query>` in command palette shows `validate_query()` results in overlay
- [ ] **P3 — Mouse:** `--mouse` enables click-to-select and scroll wheel; disabled by default
- [ ] **P3 — Themes:** `t` toggles dark/light; `--theme` flag selects at startup
- [ ] **P3 — Export:** `Ctrl+E` writes current view data to `/tmp/pgtrickle_export_*.json`; toast shows path
- [ ] **P3 — Bell:** `--bell` emits terminal bell on critical alerts; rate-limited to 1 per 10s
- [ ] **P3 — Auxiliary columns:** `list_auxiliary_columns()` displayed as tab in Delta Inspector

### Test Coverage

- [ ] **Fixtures complete:** Every new state struct has a builder in `test_fixtures.rs`; `sample_state_full()` includes all 15 new data fields
- [ ] **Unit tests:** ≥30 new unit tests across `state.rs` (issue detection, counters), `app.rs` (key dispatch, tab cycling, command parsing), pure logic (colors, bar widths, formatting)
- [ ] **Snapshot tests:** ≥25 new snapshots covering every new/modified view panel and rendering path; `insta review` clean
- [ ] **Contract stubs:** 17 new SQL stubs in `test_db.rs` with exact column name/type matches to `SQL_REFERENCE.md`
- [ ] **Contract tests:** 17 new `test_poll_*_executes` / `test_action_*_executes` tests passing against Testcontainers
- [ ] **Degradation tests:** ≥9 tests verifying TUI renders correctly when newer functions are absent (v0.13 compat)
- [ ] **Action lifecycle tests:** 6 tests covering confirmation dialogs, toast feedback, gate toggle logic
- [ ] **Cross-view integration:** 10 composed snapshot tests (T14-B) verifying features work together
- [ ] **Reconnect integration:** Action-during-disconnect test passes; backoff sequence verified
- [ ] **Narrow terminal:** All views render without panics at 60×20
- [ ] **Coverage:** `state.rs` ≥85%, `app.rs` ≥75%, `poller.rs` ≥65%, `views/` ≥85% (measured by `cargo tarpaulin` or `cargo llvm-cov`)

### Documentation & CI

- [ ] **Docs:** `docs/TUI.md` updated with all new flags, keybindings, and SQL functions surfaced
- [ ] **Help overlays:** Context tips updated for CDC, Diagnostics, Detail, Watermarks, and Delta Inspector views
- [ ] **CHANGELOG:** All 30 features documented
- [ ] **CI:** `just lint` zero warnings; `cargo test --workspace` green; all snapshot tests accepted
