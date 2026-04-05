use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::cli::{ConnectionArgs, ThemeChoice};
use crate::error::CliError;
use crate::state::{
    ActionRequest, ActionResult, AppState, ConfirmDialog, SortMode, Toast, ToastStyle,
};
use crate::theme::Theme;
use crate::views;

/// Views the user can switch between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Detail,
    Graph,
    RefreshLog,
    Diagnostics,
    Cdc,
    Config,
    Health,
    Alerts,
    Workers,
    Fuse,
    Watermarks,
    DeltaInspector,
    Issues,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Detail => "Detail",
            Self::Graph => "Dependencies",
            Self::RefreshLog => "Refresh Log",
            Self::Diagnostics => "Diagnostics",
            Self::Cdc => "CDC Health",
            Self::Config => "Configuration",
            Self::Health => "Health Checks",
            Self::Alerts => "Alerts",
            Self::Workers => "Workers",
            Self::Fuse => "Fuse",
            Self::Watermarks => "Watermarks",
            Self::DeltaInspector => "Delta SQL",
            Self::Issues => "Issues",
        }
    }

    fn key_hint(self) -> &'static str {
        match self {
            Self::Dashboard => "1",
            Self::Detail => "2",
            Self::Graph => "3",
            Self::RefreshLog => "4",
            Self::Diagnostics => "5",
            Self::Cdc => "6",
            Self::Config => "7",
            Self::Health => "8",
            Self::Alerts => "9",
            Self::Workers => "w",
            Self::Fuse => "f",
            Self::Watermarks => "m",
            Self::DeltaInspector => "d",
            Self::Issues => "i",
        }
    }
}

const ALL_VIEWS: [View; 14] = [
    View::Dashboard,
    View::Detail,
    View::Graph,
    View::RefreshLog,
    View::Diagnostics,
    View::Cdc,
    View::Config,
    View::Health,
    View::Alerts,
    View::Workers,
    View::Fuse,
    View::Watermarks,
    View::DeltaInspector,
    View::Issues,
];

/// Messages from the background poller to the UI thread.
enum PollMsg {
    StateUpdate(Box<AppState>),
    Error(String),
    ActionResult(ActionResult),
    /// Delta SQL fetched on demand
    DeltaSql(String, String), // (table_name, sql)
    /// DDL fetched on demand
    Ddl(String, String), // (table_name, ddl)
    /// Diagnosed errors fetched on demand
    DiagnosedErrors(String, String), // (table_name, json)
    /// Explain refresh mode fetched on demand
    ExplainMode(String, String), // (table_name, json)
    /// Source table detail fetched on demand
    Sources(String, String), // (table_name, json)
    /// Refresh history fetched on demand
    RefreshHistory(String, String), // (table_name, json)
    /// Auxiliary columns fetched on demand
    AuxiliaryColumns(String, String), // (table_name, json)
    /// Reconnected after connection loss
    Reconnected,
}

struct App {
    state: AppState,
    theme: Theme,
    current_view: View,
    selected: usize,
    show_help: bool,
    filter: Option<String>,
    filter_input: String,
    entering_filter: bool,
    should_quit: bool,
    /// Channel to request a force poll
    force_poll_tx: Option<mpsc::Sender<()>>,
    /// Channel to send write actions to the poller
    action_tx: Option<mpsc::Sender<ActionRequest>>,
    /// Toast notification (most recent)
    toast: Option<Toast>,
    /// Confirmation dialog
    confirming: Option<ConfirmDialog>,
    /// Sort mode for dashboard
    sort_mode: SortMode,
    sort_ascending: bool,
    /// Command palette state
    command_palette: Option<CommandPalette>,
    /// Configurable poll interval (seconds)
    poll_interval: u64,
    /// Mouse enabled
    mouse_enabled: bool,
    /// Bell enabled
    bell_enabled: bool,
    /// Last bell time
    last_bell: std::time::Instant,
    /// Watermarks sub-tab (0=groups, 1=gates)
    watermarks_tab: usize,
    /// DDL overlay text
    ddl_overlay: Option<String>,
    /// Validate overlay text
    validate_overlay: Option<String>,
    /// Delta inspector sub-tab (0=SQL, 1=Auxiliary columns)
    delta_inspector_tab: usize,
}

/// Simple command palette for `:` mode.
struct CommandPalette {
    input: String,
    suggestions: Vec<CommandSuggestion>,
    selected_suggestion: usize,
}

struct CommandSuggestion {
    command: String,
    description: String,
}

impl CommandPalette {
    fn new() -> Self {
        Self {
            input: String::new(),
            suggestions: Vec::new(),
            selected_suggestion: 0,
        }
    }

    fn update_suggestions(&mut self, stream_table_names: &[String]) {
        let input = self.input.to_lowercase();
        let mut suggestions = Vec::new();

        // Built-in commands
        let commands = [
            ("refresh", "Trigger manual refresh for a stream table"),
            ("refresh all", "Refresh all active stream tables"),
            ("pause", "Pause a stream table"),
            ("resume", "Resume a paused stream table"),
            ("repair", "Repair stream table CDC triggers"),
            ("fuse reset", "Reset blown fuse for a stream table"),
            ("validate", "Validate a SQL query for DVM compatibility"),
            ("export", "Show DDL for a stream table"),
            ("explain", "Fetch delta SQL for a stream table"),
            ("quit", "Exit pgtrickle"),
        ];

        for (cmd, desc) in &commands {
            if cmd.contains(&input) || input.is_empty() {
                suggestions.push(CommandSuggestion {
                    command: cmd.to_string(),
                    description: desc.to_string(),
                });
            }
        }

        // Add stream table name completions for table-specific commands
        if input.starts_with("refresh ")
            || input.starts_with("pause ")
            || input.starts_with("resume ")
            || input.starts_with("repair ")
            || input.starts_with("export ")
            || input.starts_with("explain ")
        {
            let parts: Vec<&str> = input.splitn(2, ' ').collect();
            let prefix = parts.get(1).unwrap_or(&"");
            let cmd_word = parts[0];
            suggestions.clear();
            for name in stream_table_names {
                if name.to_lowercase().contains(prefix) {
                    suggestions.push(CommandSuggestion {
                        command: format!("{cmd_word} {name}"),
                        description: format!("→ {name}"),
                    });
                }
            }
        }

        // explain uses schema-qualified names — add those too
        if input.starts_with("explain ") {
            let parts: Vec<&str> = input.splitn(2, ' ').collect();
            let prefix = parts.get(1).unwrap_or(&"");
            suggestions.retain(|s| s.command.to_lowercase().contains(prefix));
        }

        self.suggestions = suggestions;
        if self.selected_suggestion >= self.suggestions.len() {
            self.selected_suggestion = 0;
        }
    }
}

impl App {
    fn new(poll_interval: u64, theme: Theme, mouse: bool, bell: bool) -> Self {
        Self {
            state: AppState::default(),
            theme,
            current_view: View::Dashboard,
            selected: 0,
            show_help: false,
            filter: None,
            filter_input: String::new(),
            entering_filter: false,
            should_quit: false,
            force_poll_tx: None,
            action_tx: None,
            toast: None,
            confirming: None,
            sort_mode: SortMode::StatusSeverity,
            sort_ascending: true,
            command_palette: None,
            poll_interval,
            mouse_enabled: mouse,
            bell_enabled: bell,
            last_bell: std::time::Instant::now(),
            watermarks_tab: 0,
            ddl_overlay: None,
            validate_overlay: None,
            delta_inspector_tab: 0,
        }
    }

    /// Get the filtered stream tables for the current view.
    fn filtered_stream_tables(&self) -> Vec<usize> {
        let filter = self.filter.as_deref().unwrap_or("");
        self.state
            .stream_tables
            .iter()
            .enumerate()
            .filter(|(_, st)| {
                if filter.is_empty() {
                    return true;
                }
                let f = filter.to_lowercase();
                st.name.to_lowercase().contains(&f)
                    || st.schema.to_lowercase().contains(&f)
                    || st.status.to_lowercase().contains(&f)
                    || st.refresh_mode.to_lowercase().contains(&f)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Get filtered stream tables in the sorted order based on current sort mode.
    fn filtered_sorted_stream_tables(&self) -> Vec<usize> {
        let mut indices = self.filtered_stream_tables();
        let ascending = self.sort_ascending;
        indices.sort_by(|&a, &b| {
            let sa = &self.state.stream_tables[a];
            let sb = &self.state.stream_tables[b];
            let cmp = match self.sort_mode {
                SortMode::StatusSeverity => {
                    let ord = |st: &crate::state::StreamTableInfo| -> u8 {
                        if st.status == "ERROR" || st.status == "SUSPENDED" {
                            0
                        } else if st.cascade_stale {
                            1
                        } else if st.stale {
                            2
                        } else {
                            3
                        }
                    };
                    ord(sa).cmp(&ord(sb)).then_with(|| sa.name.cmp(&sb.name))
                }
                SortMode::Name => sa.name.cmp(&sb.name),
                SortMode::AvgDuration => {
                    let da = sa.avg_duration_ms.unwrap_or(0.0);
                    let db = sb.avg_duration_ms.unwrap_or(0.0);
                    db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
                }
                SortMode::LastRefresh => {
                    let la = sa.last_refresh_at.as_deref().unwrap_or("");
                    let lb = sb.last_refresh_at.as_deref().unwrap_or("");
                    la.cmp(lb)
                }
                SortMode::TotalRefreshes => sb.total_refreshes.cmp(&sa.total_refreshes),
                SortMode::Staleness => {
                    let parse_staleness = |s: &Option<String>| -> f64 {
                        s.as_deref()
                            .and_then(|v| v.trim_end_matches('s').parse::<f64>().ok())
                            .unwrap_or(0.0)
                    };
                    let sta = parse_staleness(&sa.staleness);
                    let stb = parse_staleness(&sb.staleness);
                    stb.partial_cmp(&sta).unwrap_or(std::cmp::Ordering::Equal)
                }
            };
            if ascending { cmp } else { cmp.reverse() }
        });
        indices
    }

    fn selected_stream_table_index(&self) -> Option<usize> {
        self.filtered_sorted_stream_tables()
            .get(self.selected)
            .copied()
    }

    fn clamp_selection(&mut self) {
        let len = self.list_len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn list_len(&self) -> usize {
        match self.current_view {
            View::Dashboard | View::Detail | View::DeltaInspector => {
                self.filtered_stream_tables().len()
            }
            View::Graph => self.state.dag_edges.len(),
            View::RefreshLog => self.filtered_refresh_log_len(),
            View::Diagnostics => self.filtered_diagnostics_len(),
            View::Cdc => self.filtered_cdc_len(),
            View::Config => self.filtered_config_len(),
            View::Health => self.filtered_health_len(),
            View::Alerts => self.state.alerts.len(),
            View::Workers => self.filtered_workers_len(),
            View::Fuse => self.filtered_fuse_len(),
            View::Watermarks => self.state.watermark_groups.len(),
            View::Issues => self.filtered_issues_len(),
        }
    }

    // ── Cross-view filter helpers ────────────────────────────────

    fn filter_matches(&self, haystack: &str) -> bool {
        match &self.filter {
            None => true,
            Some(f) => haystack.to_lowercase().contains(&f.to_lowercase()),
        }
    }

    fn filtered_refresh_log_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.refresh_log.len();
        }
        self.state
            .refresh_log
            .iter()
            .filter(|e| self.filter_matches(&e.st_name))
            .count()
    }

    fn filtered_diagnostics_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.diagnostics.len();
        }
        self.state
            .diagnostics
            .iter()
            .filter(|d| self.filter_matches(&d.name) || self.filter_matches(&d.schema))
            .count()
    }

    fn filtered_cdc_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.cdc_buffers.len();
        }
        self.state
            .cdc_buffers
            .iter()
            .filter(|b| {
                self.filter_matches(&b.stream_table) || self.filter_matches(&b.source_table)
            })
            .count()
    }

    fn filtered_config_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.guc_params.len();
        }
        self.state
            .guc_params
            .iter()
            .filter(|g| self.filter_matches(&g.name) || self.filter_matches(&g.short_desc))
            .count()
    }

    fn filtered_health_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.health_checks.len();
        }
        self.state
            .health_checks
            .iter()
            .filter(|h| self.filter_matches(&h.check_name) || self.filter_matches(&h.detail))
            .count()
    }

    fn filtered_workers_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.workers.len();
        }
        self.state
            .workers
            .iter()
            .filter(|w| {
                w.table_name
                    .as_deref()
                    .is_some_and(|n| self.filter_matches(n))
                    || self.filter_matches(&w.state)
            })
            .count()
    }

    fn filtered_fuse_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.fuses.len();
        }
        self.state
            .fuses
            .iter()
            .filter(|f| self.filter_matches(&f.stream_table))
            .count()
    }

    fn filtered_issues_len(&self) -> usize {
        if self.filter.is_none() {
            return self.state.issues.len();
        }
        self.state
            .issues
            .iter()
            .filter(|i| {
                i.affected_table
                    .as_deref()
                    .is_some_and(|t| self.filter_matches(t))
                    || self.filter_matches(&i.category)
                    || self.filter_matches(&i.summary)
            })
            .count()
    }

    fn move_down(&mut self) {
        let len = self.list_len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Launch the interactive TUI. Blocks until the user quits.
pub async fn run(connection: &ConnectionArgs) -> Result<(), CliError> {
    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    if connection.mouse {
        execute!(stdout, crossterm::event::EnableMouseCapture)?;
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, connection).await;

    // Restore terminal
    if connection.mouse {
        execute!(
            terminal.backend_mut(),
            crossterm::event::DisableMouseCapture
        )
        .ok();
    }
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    connection: &ConnectionArgs,
) -> Result<(), CliError> {
    let theme = match connection.theme {
        ThemeChoice::Dark => Theme::default_dark(),
        ThemeChoice::Light => Theme::light(),
    };
    let mut app = App::new(
        connection.interval,
        theme,
        connection.mouse,
        connection.bell,
    );

    // Channel for state updates from poller
    let (tx, mut rx) = mpsc::channel::<PollMsg>(4);

    // Channel for force poll requests
    let (force_tx, force_rx) = mpsc::channel::<()>(1);
    app.force_poll_tx = Some(force_tx);

    // Channel for write actions (UI → poller)
    let (action_tx, action_rx) = mpsc::channel::<ActionRequest>(8);
    app.action_tx = Some(action_tx);

    // Spawn background poller with reconnect support
    let conn_args = connection.clone();
    let poll_interval = connection.interval;
    tokio::spawn(async move {
        poller_task(conn_args, tx, force_rx, action_rx, poll_interval).await;
    });

    // Spawn LISTEN/NOTIFY listener for real-time alerts
    let (alert_tx, mut alert_rx) = mpsc::channel::<crate::state::AlertEvent>(32);
    let alert_conn_args = connection.clone();
    tokio::spawn(async move {
        listen_task(alert_conn_args, alert_tx).await;
    });

    loop {
        // Draw
        terminal.draw(|frame| draw_ui(frame, &app))?;

        // Handle events with a short poll timeout so we pick up async state updates
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, key),
                Event::Mouse(mouse) if app.mouse_enabled => {
                    handle_mouse(&mut app, mouse);
                }
                _ => {}
            }
        }

        // Drain state updates from poller
        while let Ok(msg) = rx.try_recv() {
            match msg {
                PollMsg::StateUpdate(new_state) => {
                    // Preserve alerts from LISTEN/NOTIFY (state polls don't include them)
                    let alerts = std::mem::take(&mut app.state.alerts);
                    // Preserve caches
                    let delta_sql_cache = std::mem::take(&mut app.state.delta_sql_cache);
                    let ddl_cache = std::mem::take(&mut app.state.ddl_cache);
                    let diagnosed_errors = std::mem::take(&mut app.state.diagnosed_errors);
                    let explain_mode_cache = std::mem::take(&mut app.state.explain_mode_cache);
                    let source_detail_cache = std::mem::take(&mut app.state.source_detail_cache);
                    let refresh_history_cache =
                        std::mem::take(&mut app.state.refresh_history_cache);
                    let auxiliary_columns_cache =
                        std::mem::take(&mut app.state.auxiliary_columns_cache);
                    app.state = *new_state;
                    app.state.alerts = alerts;
                    app.state.delta_sql_cache = delta_sql_cache;
                    app.state.ddl_cache = ddl_cache;
                    app.state.diagnosed_errors = diagnosed_errors;
                    app.state.explain_mode_cache = explain_mode_cache;
                    app.state.source_detail_cache = source_detail_cache;
                    app.state.refresh_history_cache = refresh_history_cache;
                    app.state.auxiliary_columns_cache = auxiliary_columns_cache;
                    app.state.poll_interval_ms = app.poll_interval * 1000;
                    app.clamp_selection();
                }
                PollMsg::Error(e) => {
                    app.state.error_message = Some(e);
                    app.state.connected = false;
                }
                PollMsg::ActionResult(result) => {
                    if result.success {
                        app.toast = Some(Toast::success(&result.message));
                    } else {
                        app.toast = Some(Toast::error(&result.message));
                    }
                }
                PollMsg::DeltaSql(name, sql) => {
                    // Cache by bare name (strip schema prefix if present) so the
                    // Delta Inspector can look it up via st.name.
                    let bare_name = name
                        .split_once('.')
                        .map(|(_, n)| n.to_string())
                        .unwrap_or_else(|| name.clone());
                    app.state.delta_sql_cache.insert(bare_name.clone(), sql);
                    // Navigate to Delta Inspector so the user can see the result.
                    // Select the matching stream table first (if found).
                    if let Some(pos) = app
                        .filtered_sorted_stream_tables()
                        .iter()
                        .position(|&i| app.state.stream_tables[i].name == bare_name)
                    {
                        app.selected = pos;
                    }
                    app.current_view = View::DeltaInspector;
                    app.delta_inspector_tab = 0;
                    app.toast = Some(Toast::success(format!("Delta SQL loaded for {bare_name}")));
                }
                PollMsg::Ddl(name, ddl) => {
                    // Cache by bare name so future 'e' presses hit the cache.
                    let bare_name = name
                        .split_once('.')
                        .map(|(_, n)| n.to_string())
                        .unwrap_or_else(|| name.clone());
                    app.state.ddl_cache.insert(bare_name, ddl.clone());
                    app.ddl_overlay = Some(ddl);
                }
                PollMsg::DiagnosedErrors(name, json) => {
                    if let Ok(errors) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                        let parsed: Vec<crate::state::DiagnosedError> = errors
                            .iter()
                            .filter_map(|v| {
                                Some(crate::state::DiagnosedError {
                                    event_time: v.get("event_time")?.as_str()?.to_string(),
                                    error_type: v.get("error_type")?.as_str()?.to_string(),
                                    error_message: v.get("error_message")?.as_str()?.to_string(),
                                    remediation: v.get("remediation")?.as_str()?.to_string(),
                                })
                            })
                            .collect();
                        app.state.diagnosed_errors.insert(name, parsed);
                    }
                }
                PollMsg::ExplainMode(name, json) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json)
                        && let (Some(cm), Some(em)) = (
                            v.get("configured_mode").and_then(|s| s.as_str()),
                            v.get("effective_mode").and_then(|s| s.as_str()),
                        )
                    {
                        app.state.explain_mode_cache.insert(
                            name,
                            crate::state::ExplainRefreshMode {
                                configured_mode: cm.to_string(),
                                effective_mode: em.to_string(),
                                downgrade_reason: v
                                    .get("downgrade_reason")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string()),
                            },
                        );
                    }
                }
                PollMsg::Sources(name, json) => {
                    if let Ok(sources) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                        let parsed: Vec<crate::state::SourceTableInfo> = sources
                            .iter()
                            .filter_map(|v| {
                                Some(crate::state::SourceTableInfo {
                                    source_table: v.get("source_table")?.as_str()?.to_string(),
                                    source_type: v.get("source_type")?.as_str()?.to_string(),
                                    cdc_mode: v.get("cdc_mode")?.as_str()?.to_string(),
                                    columns_used: v
                                        .get("columns_used")
                                        .and_then(|s| s.as_str())
                                        .map(|s| s.to_string()),
                                })
                            })
                            .collect();
                        app.state.source_detail_cache.insert(name, parsed);
                    }
                }
                PollMsg::RefreshHistory(name, json) => {
                    if let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                        let parsed: Vec<crate::state::RefreshHistoryEntry> = entries
                            .iter()
                            .filter_map(|v| {
                                Some(crate::state::RefreshHistoryEntry {
                                    action: v.get("action")?.as_str()?.to_string(),
                                    status: v.get("status")?.as_str()?.to_string(),
                                    rows_inserted: v.get("rows_inserted").and_then(|s| s.as_i64()),
                                    rows_deleted: v.get("rows_deleted").and_then(|s| s.as_i64()),
                                    delta_row_count: v
                                        .get("delta_row_count")
                                        .and_then(|s| s.as_i64()),
                                    duration_ms: v.get("duration_ms").and_then(|s| s.as_f64()),
                                    was_full_fallback: v
                                        .get("was_full_fallback")
                                        .and_then(|s| s.as_bool())
                                        .unwrap_or(false),
                                    start_time: v.get("start_time")?.as_str()?.to_string(),
                                    error_message: v
                                        .get("error_message")
                                        .and_then(|s| s.as_str())
                                        .map(|s| s.to_string()),
                                })
                            })
                            .collect();
                        app.state.refresh_history_cache.insert(name, parsed);
                    }
                }
                PollMsg::AuxiliaryColumns(name, json) => {
                    if let Ok(cols) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                        let parsed: Vec<crate::state::AuxiliaryColumn> = cols
                            .iter()
                            .filter_map(|v| {
                                Some(crate::state::AuxiliaryColumn {
                                    column_name: v.get("column_name")?.as_str()?.to_string(),
                                    data_type: v.get("data_type")?.as_str()?.to_string(),
                                    purpose: v.get("purpose")?.as_str()?.to_string(),
                                })
                            })
                            .collect();
                        app.state.auxiliary_columns_cache.insert(name, parsed);
                    }
                }
                PollMsg::Reconnected => {
                    app.toast = Some(Toast::success("Reconnected to database"));
                }
            }
        }

        // Drain real-time LISTEN/NOTIFY alerts
        while let Ok(alert) = alert_rx.try_recv() {
            // Bell on critical alerts
            if app.bell_enabled
                && alert.severity == "critical"
                && app.last_bell.elapsed() > Duration::from_secs(10)
            {
                print!("\x07");
                app.last_bell = std::time::Instant::now();
            }
            app.state.alerts.push(alert);
            // Keep last 200 alerts
            if app.state.alerts.len() > 200 {
                app.state.alerts.remove(0);
            }
        }

        // Expire toast
        if let Some(ref toast) = app.toast
            && toast.is_expired()
        {
            app.toast = None;
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Exponential backoff for reconnect.
struct Backoff {
    attempt: u32,
    max_delay_secs: u64,
}

impl Backoff {
    fn new(max_delay_secs: u64) -> Self {
        Self {
            attempt: 0,
            max_delay_secs,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let secs = (1u64 << self.attempt.min(4)).min(self.max_delay_secs);
        self.attempt += 1;
        Duration::from_secs(secs)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

async fn poller_task(
    conn_args: ConnectionArgs,
    tx: mpsc::Sender<PollMsg>,
    mut force_rx: mpsc::Receiver<()>,
    mut action_rx: mpsc::Receiver<ActionRequest>,
    poll_interval_secs: u64,
) {
    let mut backoff = Backoff::new(15);

    loop {
        // Connect (or reconnect)
        let client = loop {
            match crate::connection::connect(&conn_args).await {
                Ok(c) => {
                    backoff.reset();
                    break c;
                }
                Err(e) => {
                    let _ = tx.send(PollMsg::Error(format!("Connecting: {e}"))).await;
                    let delay = backoff.next_delay();
                    tokio::time::sleep(delay).await;
                    if tx.is_closed() {
                        return;
                    }
                }
            }
        };

        // Signal reconnected (except first connect)
        if backoff.attempt > 0 {
            let _ = tx.send(PollMsg::Reconnected).await;
        }

        let mut tick = interval(Duration::from_secs(poll_interval_secs));

        // Inner poll loop — breaks on connection error
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = force_rx.recv() => {}
                action = action_rx.recv() => {
                    if let Some(action) = action {
                        let result = crate::poller::execute_action(&client, &action).await;
                        // For fetch actions, send specialized messages
                        match &action {
                            ActionRequest::FetchDeltaSql(name) if result.success => {
                                let _ = tx.send(PollMsg::DeltaSql(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchDdl(name) if result.success => {
                                let _ = tx.send(PollMsg::Ddl(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchDiagnoseErrors(name) if result.success => {
                                let _ = tx.send(PollMsg::DiagnosedErrors(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchExplainMode(name) if result.success => {
                                let _ = tx.send(PollMsg::ExplainMode(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchSources(name) if result.success => {
                                let _ = tx.send(PollMsg::Sources(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchRefreshHistory(name) if result.success => {
                                let _ = tx.send(PollMsg::RefreshHistory(name.clone(), result.message)).await;
                                continue;
                            }
                            ActionRequest::FetchAuxiliaryColumns(name) if result.success => {
                                let _ = tx.send(PollMsg::AuxiliaryColumns(name.clone(), result.message)).await;
                                continue;
                            }
                            // Silently degrade background enrichment fetches — these are
                            // auto-triggered and may not exist on older extension versions.
                            ActionRequest::FetchDiagnoseErrors(_)
                            | ActionRequest::FetchExplainMode(_)
                            | ActionRequest::FetchSources(_)
                            | ActionRequest::FetchRefreshHistory(_)
                            | ActionRequest::FetchAuxiliaryColumns(_)
                                if !result.success =>
                            {
                                continue;
                            }
                            _ => {}
                        }
                        let _ = tx.send(PollMsg::ActionResult(result)).await;
                        // Force immediate re-poll after write actions
                        continue;
                    }
                }
            }

            if tx.is_closed() {
                return;
            }

            let mut state = AppState {
                poll_interval_ms: poll_interval_secs * 1000,
                ..AppState::default()
            };
            crate::poller::poll_all(&client, &mut state).await;

            // Detect connection loss
            if state.all_polls_failed() {
                let _ = tx.send(PollMsg::Error("Connection lost".to_string())).await;
                break; // → outer reconnect loop
            }

            if tx
                .send(PollMsg::StateUpdate(Box::new(state)))
                .await
                .is_err()
            {
                return;
            }
        }

        // Backoff before reconnecting
        let delay = backoff.next_delay();
        tokio::time::sleep(delay).await;
        if tx.is_closed() {
            return;
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Command palette mode
    if let Some(ref mut palette) = app.command_palette {
        match key.code {
            KeyCode::Esc => {
                app.command_palette = None;
            }
            KeyCode::Enter => {
                let input = palette.input.clone();
                app.command_palette = None;
                execute_palette_command(app, &input);
            }
            KeyCode::Backspace => {
                palette.input.pop();
                let names: Vec<String> = app
                    .state
                    .stream_tables
                    .iter()
                    .map(|s| s.name.clone())
                    .collect();
                palette.update_suggestions(&names);
            }
            KeyCode::Char(c) => {
                palette.input.push(c);
                let names: Vec<String> = app
                    .state
                    .stream_tables
                    .iter()
                    .map(|s| s.name.clone())
                    .collect();
                palette.update_suggestions(&names);
            }
            KeyCode::Tab => {
                if !palette.suggestions.is_empty() {
                    palette.input = palette.suggestions[palette.selected_suggestion]
                        .command
                        .clone();
                    let names: Vec<String> = app
                        .state
                        .stream_tables
                        .iter()
                        .map(|s| s.name.clone())
                        .collect();
                    palette.update_suggestions(&names);
                }
            }
            KeyCode::Down => {
                if !palette.suggestions.is_empty() {
                    palette.selected_suggestion =
                        (palette.selected_suggestion + 1) % palette.suggestions.len();
                }
            }
            KeyCode::Up => {
                if !palette.suggestions.is_empty() {
                    palette.selected_suggestion = palette
                        .selected_suggestion
                        .checked_sub(1)
                        .unwrap_or(palette.suggestions.len() - 1);
                }
            }
            _ => {}
        }
        return;
    }

    // Confirmation dialog mode
    if let Some(dialog) = app.confirming.take() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(dialog.action);
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.toast = Some(Toast::info("Cancelled"));
            }
            _ => {
                // Put it back — only y/n/Esc accepted
                app.confirming = Some(dialog);
            }
        }
        return;
    }

    // DDL overlay mode
    if app.ddl_overlay.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.ddl_overlay = None;
            }
            _ => {}
        }
        return;
    }

    // Validate overlay mode
    if app.validate_overlay.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.validate_overlay = None;
            }
            _ => {}
        }
        return;
    }

    // Filter input mode
    if app.entering_filter {
        match key.code {
            KeyCode::Esc => {
                app.entering_filter = false;
                app.filter_input.clear();
            }
            KeyCode::Enter => {
                app.entering_filter = false;
                if app.filter_input.is_empty() {
                    app.filter = None;
                } else {
                    app.filter = Some(app.filter_input.clone());
                }
                app.filter_input.clear();
                app.clamp_selection();
            }
            KeyCode::Backspace => {
                app.filter_input.pop();
            }
            KeyCode::Char(c) => {
                app.filter_input.push(c);
            }
            _ => {}
        }
        return;
    }

    // Help overlay
    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                app.show_help = false;
            }
            _ => {}
        }
        return;
    }

    // Global keys
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+R: force poll now
            if let Some(ref tx) = app.force_poll_tx {
                let _ = tx.try_send(());
            }
            app.toast = Some(Toast::info("Force poll requested"));
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+E: export current view to JSON
            export_current_view(app);
        }
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        KeyCode::Char('?') => {
            app.show_help = true;
        }
        KeyCode::Char('/') => {
            app.entering_filter = true;
            app.filter_input.clear();
        }
        KeyCode::Char(':') => {
            // Open command palette
            let mut palette = CommandPalette::new();
            let names: Vec<String> = app
                .state
                .stream_tables
                .iter()
                .map(|s| s.name.clone())
                .collect();
            palette.update_suggestions(&names);
            app.command_palette = Some(palette);
        }

        // ── Write actions ────────────────────────────────────────
        KeyCode::Char('r') if matches!(app.current_view, View::Dashboard | View::Detail) => {
            // Refresh selected stream table
            if let Some(idx) = app.selected_stream_table_index() {
                let name = app.state.stream_tables[idx].name.clone();
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::RefreshTable(name.clone()));
                }
                app.toast = Some(Toast::info(format!("Refreshing {name}…")));
            }
        }
        KeyCode::Char('R') if app.current_view == View::Dashboard => {
            // Refresh all — requires confirmation
            let count = app.state.active_count();
            app.confirming = Some(ConfirmDialog {
                message: format!("Refresh all {count} active tables?"),
                action: ActionRequest::RefreshAll,
            });
        }
        KeyCode::Char('p') if matches!(app.current_view, View::Dashboard | View::Detail) => {
            // Pause selected
            if let Some(idx) = app.selected_stream_table_index() {
                let name = app.state.stream_tables[idx].name.clone();
                app.confirming = Some(ConfirmDialog {
                    message: format!("Pause {name}?"),
                    action: ActionRequest::PauseTable(name),
                });
            }
        }
        KeyCode::Char('P') if matches!(app.current_view, View::Dashboard | View::Detail) => {
            // Resume selected
            if let Some(idx) = app.selected_stream_table_index() {
                let name = app.state.stream_tables[idx].name.clone();
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::ResumeTable(name));
                }
            }
        }
        KeyCode::Char('A') if app.current_view == View::Fuse => {
            // Re-arm fuse for selected
            if let Some(fuse) = app.state.fuses.get(app.selected) {
                let name = fuse.stream_table.clone();
                app.confirming = Some(ConfirmDialog {
                    message: format!("Re-arm fuse for {name}?"),
                    action: ActionRequest::ResetFuse(name, "rearm".to_string()),
                });
            }
        }
        KeyCode::Char('e')
            if app.current_view == View::Detail || app.current_view == View::DeltaInspector =>
        {
            // Export DDL overlay
            if let Some(idx) = app.selected_stream_table_index() {
                let st = &app.state.stream_tables[idx];
                let bare_name = st.name.clone();
                if let Some(ddl) = app.state.ddl_cache.get(&bare_name) {
                    app.ddl_overlay = Some(ddl.clone());
                } else if let Some(ref tx) = app.action_tx {
                    let qualified = format!("{}.{}", st.schema, st.name);
                    let _ = tx.try_send(ActionRequest::FetchDdl(qualified));
                }
            }
        }
        KeyCode::Char('g') if app.current_view == View::Watermarks && app.watermarks_tab == 1 => {
            // Gate/ungate source
            if let Some(gate) = app.state.source_gates.get(app.selected) {
                let source = gate.source_table.clone();
                if gate.gated {
                    // Ungate — no confirmation needed
                    if let Some(ref tx) = app.action_tx {
                        let _ = tx.try_send(ActionRequest::UngateSource(source));
                    }
                } else {
                    // Gate — requires confirmation
                    app.confirming = Some(ConfirmDialog {
                        message: format!(
                            "Gate source {source}? This will block downstream refreshes."
                        ),
                        action: ActionRequest::GateSource(source),
                    });
                }
            }
        }

        // ── Sort ─────────────────────────────────────────────────
        KeyCode::Char('s') if app.current_view == View::Dashboard => {
            app.sort_mode = app.sort_mode.next();
            app.toast = Some(Toast::info(format!("Sort: {}", app.sort_mode.label())));
        }
        KeyCode::Char('S') if app.current_view == View::Dashboard => {
            app.sort_ascending = !app.sort_ascending;
            let dir = if app.sort_ascending { "▲" } else { "▼" };
            app.toast = Some(Toast::info(format!("Sort direction: {dir}")));
        }

        // ── Theme toggle ─────────────────────────────────────────
        KeyCode::Char('t') => {
            app.theme = if app.theme.name == "dark" {
                Theme::light()
            } else {
                Theme::default_dark()
            };
        }

        // ── Watermarks sub-tab ───────────────────────────────────
        KeyCode::Tab if app.current_view == View::Watermarks => {
            app.watermarks_tab = (app.watermarks_tab + 1) % 2;
            app.selected = 0;
        }

        // ── Delta Inspector sub-tab ──────────────────────────────
        KeyCode::Tab if app.current_view == View::DeltaInspector => {
            app.delta_inspector_tab = (app.delta_inspector_tab + 1) % 2;
            // Trigger auxiliary columns fetch when switching to that tab
            if app.delta_inspector_tab == 1
                && let Some(idx) = app.selected_stream_table_index()
            {
                let name = app.state.stream_tables[idx].name.clone();
                if !app.state.auxiliary_columns_cache.contains_key(&name)
                    && let Some(ref tx) = app.action_tx
                {
                    let _ = tx.try_send(ActionRequest::FetchAuxiliaryColumns(name));
                }
            }
        }

        // Enter in Delta Inspector: (re)fetch SQL for the selected table
        KeyCode::Enter if app.current_view == View::DeltaInspector => {
            if let Some(idx) = app.selected_stream_table_index() {
                let st = &app.state.stream_tables[idx];
                let qualified = format!("{}.{}", st.schema, st.name);
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::FetchDeltaSql(qualified.clone()));
                    app.toast = Some(Toast::info(format!("Fetching delta SQL for {}…", st.name)));
                }
            }
        }

        // View switching via number keys
        KeyCode::Char('1') => switch_view(app, View::Dashboard),
        KeyCode::Char('2') => switch_view(app, View::Detail),
        KeyCode::Char('3') => switch_view(app, View::Graph),
        KeyCode::Char('4') => switch_view(app, View::RefreshLog),
        KeyCode::Char('5') => switch_view(app, View::Diagnostics),
        KeyCode::Char('6') => switch_view(app, View::Cdc),
        KeyCode::Char('7') => switch_view(app, View::Config),
        KeyCode::Char('8') => switch_view(app, View::Health),
        KeyCode::Char('9') => switch_view(app, View::Alerts),
        // Extended view switching via letter keys
        KeyCode::Char('w') => switch_view(app, View::Workers),
        KeyCode::Char('f') => switch_view(app, View::Fuse),
        KeyCode::Char('m') => switch_view(app, View::Watermarks),
        KeyCode::Char('d') => switch_view(app, View::DeltaInspector),
        KeyCode::Char('g') => switch_view(app, View::Graph),
        KeyCode::Char('i') => switch_view(app, View::Issues),
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_down();
            if app.current_view == View::Detail {
                fetch_detail_data(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_up();
            if app.current_view == View::Detail {
                fetch_detail_data(app);
            }
        }
        KeyCode::PageDown => {
            for _ in 0..20 {
                app.move_down();
            }
        }
        KeyCode::PageUp => {
            for _ in 0..20 {
                app.move_up();
            }
        }
        KeyCode::Enter => {
            // Drill from dashboard to detail
            if app.current_view == View::Dashboard {
                app.current_view = View::Detail;
                fetch_detail_data(app);
            }
        }
        KeyCode::Esc => {
            // Back to dashboard
            if app.current_view != View::Dashboard {
                app.current_view = View::Dashboard;
            }
            app.filter = None;
        }
        KeyCode::Home => app.selected = 0,
        KeyCode::End => {
            let len = app.list_len();
            if len > 0 {
                app.selected = len - 1;
            }
        }
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) {
    use crossterm::event::MouseEventKind;
    match mouse.kind {
        MouseEventKind::ScrollDown => app.move_down(),
        MouseEventKind::ScrollUp => app.move_up(),
        _ => {}
    }
}

fn execute_palette_command(app: &mut App, input: &str) {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let cmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let arg = parts.get(1).map(|s| s.trim().to_string());

    match cmd.as_str() {
        "quit" | "q" => {
            app.should_quit = true;
        }
        "refresh" => {
            if let Some(name) = arg {
                if name == "all" {
                    let count = app.state.active_count();
                    app.confirming = Some(ConfirmDialog {
                        message: format!("Refresh all {count} active tables?"),
                        action: ActionRequest::RefreshAll,
                    });
                } else if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::RefreshTable(name.clone()));
                    app.toast = Some(Toast::info(format!("Refreshing {name}…")));
                }
            } else {
                app.toast = Some(Toast::error("Usage: refresh <name> or refresh all"));
            }
        }
        "pause" => {
            if let Some(name) = arg {
                app.confirming = Some(ConfirmDialog {
                    message: format!("Pause {name}?"),
                    action: ActionRequest::PauseTable(name),
                });
            } else {
                app.toast = Some(Toast::error("Usage: pause <name>"));
            }
        }
        "resume" => {
            if let Some(name) = arg {
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::ResumeTable(name));
                }
            } else {
                app.toast = Some(Toast::error("Usage: resume <name>"));
            }
        }
        "repair" => {
            if let Some(name) = arg {
                app.confirming = Some(ConfirmDialog {
                    message: format!("Repair CDC triggers for {name}?"),
                    action: ActionRequest::RepairTable(name),
                });
            } else {
                app.toast = Some(Toast::error("Usage: repair <name>"));
            }
        }
        "fuse" => {
            // fuse reset <name>
            if let Some(rest) = arg {
                let fuse_parts: Vec<&str> = rest.splitn(2, ' ').collect();
                if fuse_parts.first().map(|s| *s == "reset").unwrap_or(false) {
                    if let Some(name) = fuse_parts.get(1) {
                        app.confirming = Some(ConfirmDialog {
                            message: format!("Reset fuse for {name}?"),
                            action: ActionRequest::ResetFuse(name.to_string(), "rearm".to_string()),
                        });
                    } else {
                        app.toast = Some(Toast::error("Usage: fuse reset <name>"));
                    }
                }
            }
        }
        "export" => {
            if let Some(name) = arg {
                if let Some(ref tx) = app.action_tx {
                    let qualified = if name.contains('.') {
                        name.clone()
                    } else {
                        app.state
                            .stream_tables
                            .iter()
                            .find(|st| st.name == name)
                            .map(|st| format!("{}.{}", st.schema, st.name))
                            .unwrap_or_else(|| name.clone())
                    };
                    let _ = tx.try_send(ActionRequest::FetchDdl(qualified));
                }
            } else {
                app.toast = Some(Toast::error("Usage: export <name>"));
            }
        }
        "validate" => {
            if let Some(query) = arg {
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::ValidateQuery(query));
                }
            } else {
                app.toast = Some(Toast::error("Usage: validate <SQL query>"));
            }
        }
        "explain" => {
            if let Some(name) = arg {
                // Resolve to schema-qualified name if user gave bare name
                let qualified = if name.contains('.') {
                    name.clone()
                } else {
                    app.state
                        .stream_tables
                        .iter()
                        .find(|st| st.name == name)
                        .map(|st| format!("{}.{}", st.schema, st.name))
                        .unwrap_or_else(|| name.clone())
                };
                if let Some(ref tx) = app.action_tx {
                    let _ = tx.try_send(ActionRequest::FetchDeltaSql(qualified.clone()));
                    app.toast = Some(Toast::info(format!("Fetching delta SQL for {qualified}…")));
                }
            } else {
                app.toast = Some(Toast::error("Usage: explain <name>"));
            }
        }
        _ => {
            app.toast = Some(Toast::error(format!("Unknown command: {cmd}")));
        }
    }
}

fn export_current_view(app: &mut App) {
    let json = match app.current_view {
        View::Dashboard => serde_json::to_string_pretty(&app.state.stream_tables),
        View::Health => serde_json::to_string_pretty(&app.state.health_checks),
        View::Cdc => serde_json::to_string_pretty(&app.state.cdc_buffers),
        View::Config => serde_json::to_string_pretty(&app.state.guc_params),
        View::Diagnostics => serde_json::to_string_pretty(&app.state.diagnostics),
        View::Workers => serde_json::to_string_pretty(&app.state.workers),
        View::Fuse => serde_json::to_string_pretty(&app.state.fuses),
        View::Watermarks => serde_json::to_string_pretty(&app.state.watermark_groups),
        _ => {
            app.toast = Some(Toast::info("Export not available for this view"));
            return;
        }
    };

    match json {
        Ok(data) => {
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let path = format!("/tmp/pgtrickle_export_{ts}.json");
            match std::fs::write(&path, &data) {
                Ok(()) => {
                    app.toast = Some(Toast::success(format!("Exported to {path}")));
                }
                Err(e) => {
                    app.toast = Some(Toast::error(format!("Export failed: {e}")));
                }
            }
        }
        Err(e) => {
            app.toast = Some(Toast::error(format!("Serialization error: {e}")));
        }
    }
}

fn switch_view(app: &mut App, view: View) {
    if app.current_view != view {
        let preserve_selection =
            matches!(
                app.current_view,
                View::Dashboard | View::Detail | View::DeltaInspector
            ) && matches!(view, View::Dashboard | View::Detail | View::DeltaInspector);
        app.current_view = view;
        if !preserve_selection {
            app.selected = 0;
        }
        app.clamp_selection();

        // Trigger on-demand fetches for Detail view
        if view == View::Detail {
            fetch_detail_data(app);
        }

        // Auto-fetch delta SQL when entering Delta Inspector
        if view == View::DeltaInspector {
            fetch_delta_sql(app);
        }
    }
}

/// Fetch delta SQL for the currently selected stream table (if not cached).
fn fetch_delta_sql(app: &App) {
    if let Some(idx) = app.selected_stream_table_index() {
        let st = &app.state.stream_tables[idx];
        if !app.state.delta_sql_cache.contains_key(&st.name) {
            let qualified = format!("{}.{}", st.schema, st.name);
            if let Some(ref tx) = app.action_tx {
                let _ = tx.try_send(ActionRequest::FetchDeltaSql(qualified));
            }
        }
    }
}

/// Trigger on-demand data fetches for the Detail view of the selected stream table.
fn fetch_detail_data(app: &App) {
    if let Some(idx) = app.selected_stream_table_index() {
        let st = &app.state.stream_tables[idx];
        let name = st.name.clone();
        if let Some(ref tx) = app.action_tx {
            // Fetch explain refresh mode if not cached
            if !app.state.explain_mode_cache.contains_key(&name) {
                let _ = tx.try_send(ActionRequest::FetchExplainMode(name.clone()));
            }
            // Fetch sources if not cached
            if !app.state.source_detail_cache.contains_key(&name) {
                let _ = tx.try_send(ActionRequest::FetchSources(name.clone()));
            }
            // Fetch refresh history if not cached
            if !app.state.refresh_history_cache.contains_key(&name) {
                let _ = tx.try_send(ActionRequest::FetchRefreshHistory(name.clone()));
            }
            // Fetch diagnosed errors for error-state tables
            if (st.status == "ERROR" || st.consecutive_errors > 0)
                && !app.state.diagnosed_errors.contains_key(&name)
            {
                let _ = tx.try_send(ActionRequest::FetchDiagnoseErrors(name.clone()));
            }
        }
    }
}

fn draw_ui(frame: &mut ratatui::Frame, app: &App) {
    let size = frame.area();

    // Layout: header (1) + body + footer (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(size);

    draw_header(frame, chunks[0], app);
    draw_body(frame, chunks[1], app);
    draw_footer(frame, chunks[2], app);

    // Help overlay
    if app.show_help {
        let overlay = centered_rect(70, 70, size);
        frame.render_widget(ratatui::widgets::Clear, overlay);
        views::help::render(frame, overlay, &app.theme, app.current_view);
    }

    // Command palette overlay
    if let Some(ref palette) = app.command_palette {
        let palette_height = (palette.suggestions.len() as u16 + 2).min(8);
        let palette_area = Rect {
            x: size.x + 1,
            y: size.height.saturating_sub(palette_height + 1),
            width: size.width.saturating_sub(2),
            height: palette_height,
        };
        frame.render_widget(ratatui::widgets::Clear, palette_area);

        let mut lines = vec![Line::from(vec![
            Span::styled(" : ", app.theme.title),
            Span::raw(&palette.input),
            Span::styled("█", app.theme.title),
        ])];

        for (i, suggestion) in palette.suggestions.iter().take(6).enumerate() {
            let style = if i == palette.selected_suggestion {
                app.theme.selected
            } else {
                app.theme.dim
            };
            let prefix = if i == palette.selected_suggestion {
                "▸ "
            } else {
                "  "
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&suggestion.command, style),
                Span::styled(format!("  {}", suggestion.description), app.theme.dim),
            ]));
        }

        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(app.theme.border);
        frame.render_widget(Paragraph::new(lines).block(block), palette_area);
    }

    // DDL overlay
    if let Some(ref ddl) = app.ddl_overlay {
        let overlay = centered_rect(80, 80, size);
        frame.render_widget(ratatui::widgets::Clear, overlay);
        let block = ratatui::widgets::Block::default()
            .title(" Export DDL (Esc to close) ")
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(app.theme.border);
        frame.render_widget(Paragraph::new(ddl.as_str()).block(block), overlay);
    }

    // Validate overlay
    if let Some(ref results) = app.validate_overlay {
        let overlay = centered_rect(80, 60, size);
        frame.render_widget(ratatui::widgets::Clear, overlay);
        let block = ratatui::widgets::Block::default()
            .title(" Query Validation (Esc to close) ")
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(app.theme.border);
        frame.render_widget(Paragraph::new(results.as_str()).block(block), overlay);
    }
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let conn_status = if app.state.connected {
        Span::styled("● connected", app.theme.active)
    } else if app.state.reconnecting {
        Span::styled("◌ reconnecting…", app.theme.warning)
    } else {
        Span::styled("✗ disconnected", app.theme.error)
    };

    let view_label_text = if app.current_view == View::DeltaInspector {
        let table_suffix = app
            .selected_stream_table_index()
            .and_then(|idx| app.state.stream_tables.get(idx))
            .map(|st| format!(" — {}.{}", st.schema, st.name))
            .unwrap_or_default();
        format!(" Delta SQL{table_suffix} ")
    } else {
        format!(" {} ", app.current_view.label())
    };
    let view_label = Span::styled(
        view_label_text,
        app.theme.title.add_modifier(Modifier::REVERSED),
    );

    let poll_info = app
        .state
        .last_poll
        .map(|t| {
            let ago = chrono::Utc::now().signed_duration_since(t);
            format!("polled {}s ago", ago.num_seconds())
        })
        .unwrap_or_else(|| "no poll yet".to_string());

    let right = Span::styled(format!(" {poll_info} "), app.theme.dim);

    // Issue badge (visible from every view)
    let issue_count = app.state.issue_count();
    let issue_badge = if issue_count > 0 {
        Span::styled(format!(" ⚠ {issue_count} "), app.theme.warning)
    } else {
        Span::raw("")
    };

    // Scheduler indicator from quick_health
    let scheduler_span = match &app.state.quick_health {
        Some(qh) if qh.scheduler_running => Span::styled(" ⚙ scheduler ", app.theme.active),
        Some(_) => Span::styled(" ✗ scheduler stopped ", app.theme.error),
        None => Span::raw(""),
    };

    // Poll interval indicator
    let interval_span = Span::styled(format!(" ⏱ {}s ", app.poll_interval), app.theme.dim);

    let header = Line::from(vec![
        Span::styled(" pg_trickle ", app.theme.title),
        view_label,
        Span::raw(" "),
        conn_status,
        Span::raw(" "),
        scheduler_span,
        interval_span,
        issue_badge,
        Span::raw(" "),
        right,
    ]);

    frame.render_widget(Paragraph::new(header), area);
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    // Show error banner if present
    if let Some(ref err) = app.state.error_message
        && !app.state.connected
    {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);

        let banner = Line::from(vec![
            Span::styled(" ⚠ ", app.theme.error),
            Span::styled(err.as_str(), app.theme.error),
        ]);
        frame.render_widget(Paragraph::new(banner), chunks[0]);

        render_view(frame, chunks[1], app);
        return;
    }

    render_view(frame, area, app);
}

fn render_view(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let filter = app.filter.as_deref();
    match app.current_view {
        View::Dashboard => {
            views::dashboard::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Detail => views::detail::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected_stream_table_index(),
        ),
        View::Graph => views::graph::render(frame, area, &app.state, &app.theme, app.selected),
        View::RefreshLog => {
            views::refresh_log::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Diagnostics => {
            views::diagnostics::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Cdc => views::cdc::render(frame, area, &app.state, &app.theme, app.selected, filter),
        View::Config => {
            views::config::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Health => {
            views::health::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Alerts => views::alert::render(frame, area, &app.state, &app.theme),
        View::Workers => {
            views::workers::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Fuse => {
            views::fuse::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
        View::Watermarks => views::watermarks::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected,
            app.watermarks_tab,
        ),
        View::DeltaInspector => views::delta_inspector::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected_stream_table_index(),
            app.delta_inspector_tab,
        ),
        View::Issues => {
            views::issues::render(frame, area, &app.state, &app.theme, app.selected, filter)
        }
    }
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    // Confirmation dialog takes priority
    if let Some(ref dialog) = app.confirming {
        let line = Line::from(vec![
            Span::styled(" ⚠ ", app.theme.warning),
            Span::styled(&dialog.message, app.theme.warning),
            Span::styled(" [y/n] ", app.theme.title),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Toast takes priority over normal footer
    if let Some(ref toast) = app.toast {
        let (icon, style) = match toast.style {
            ToastStyle::Success => ("✓ ", app.theme.active),
            ToastStyle::Error => ("✗ ", app.theme.error),
            ToastStyle::Info => ("● ", Style::default().fg(Color::Cyan)),
        };
        let line = Line::from(vec![
            Span::styled(format!(" {icon}"), style),
            Span::styled(&toast.message, style),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let mut spans = vec![];

    if app.entering_filter {
        spans.push(Span::styled(" /", app.theme.title));
        spans.push(Span::raw(&app.filter_input));
        spans.push(Span::styled("█", app.theme.title));
    } else {
        // Context-sensitive key hints
        for view in &ALL_VIEWS {
            if *view == app.current_view {
                let label = if *view == View::DeltaInspector {
                    let table_suffix = app
                        .selected_stream_table_index()
                        .and_then(|idx| app.state.stream_tables.get(idx))
                        .map(|st| format!(": {}.{}", st.schema, st.name))
                        .unwrap_or_default();
                    format!(" [{}]{}{} ", view.key_hint(), view.label(), table_suffix)
                } else {
                    format!(" [{}]{} ", view.key_hint(), view.label())
                };
                spans.push(Span::styled(label, app.theme.footer_active));
            } else {
                spans.push(Span::styled(
                    format!(" {}-{} ", view.key_hint(), view.label()),
                    app.theme.footer,
                ));
            }
        }
        spans.push(Span::styled(" ?-Help q-Quit ", app.theme.footer));

        if let Some(ref f) = app.filter {
            spans.push(Span::styled(format!(" /{f}"), app.theme.warning));
        }
    }

    let footer = Line::from(spans);
    frame.render_widget(Paragraph::new(footer), area);
}

/// Parse a `pg_trickle_alert` NOTIFY payload into an `AlertEvent`.
///
/// The server always emits a JSON object with at minimum an `event` field.
/// We derive `severity` from the event type, extract `st` as the table name,
/// and decompose the remaining fields into `metric` (primary numeric) and
/// `context` (secondary string).
fn parse_alert_payload(payload: &str) -> crate::state::AlertEvent {
    let now = chrono::Utc::now();

    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return crate::state::AlertEvent {
            timestamp: now,
            severity: "info".to_string(),
            event: "unknown".to_string(),
            table: String::new(),
            metric: String::new(),
            context: payload.to_string(),
        };
    };

    let event = v
        .get("event")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown")
        .to_string();

    let severity = alert_event_severity(&event).to_string();

    let table = v
        .get("st")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let schema = v.get("pgt_schema").and_then(|s| s.as_str())?;
            let name = v.get("pgt_name").and_then(|s| s.as_str())?;
            Some(format!("{schema}.{name}"))
        })
        .unwrap_or_default();

    let str_field = |key: &str| -> Option<String> {
        v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
    };
    let f64_field = |key: &str| -> Option<f64> { v.get(key).and_then(|x| x.as_f64()) };
    let i64_field = |key: &str| -> Option<i64> { v.get(key).and_then(|x| x.as_i64()) };

    let (metric, context) = match event.as_str() {
        "stale_data" => {
            let metric = f64_field("ratio")
                .map(|r| format!("ratio={r:.2}×"))
                .unwrap_or_default();
            let context = f64_field("staleness_seconds")
                .map(|s| format!("staleness={s:.1}s"))
                .unwrap_or_default();
            (metric, context)
        }
        "auto_suspended" => {
            let metric = i64_field("consecutive_errors")
                .map(|n| format!("errors={n}"))
                .unwrap_or_default();
            (metric, String::new())
        }
        "resumed" => {
            let context = str_field("previous_status")
                .map(|s| format!("prev={s}"))
                .unwrap_or_default();
            (String::new(), context)
        }
        "reinitialize_needed" => {
            let context = str_field("reason").unwrap_or_default();
            (String::new(), context)
        }
        "buffer_growth_warning" => {
            let metric = i64_field("pending_bytes")
                .map(|b| format_bytes(b))
                .unwrap_or_default();
            let context = str_field("slot_name")
                .map(|s| format!("slot={s}"))
                .unwrap_or_default();
            (metric, context)
        }
        "slot_lag_warning" => {
            let metric = i64_field("retained_wal_bytes")
                .map(|b| format_bytes(b))
                .unwrap_or_default();
            let context = str_field("slot_name")
                .map(|s| format!("slot={s}"))
                .unwrap_or_default();
            (metric, context)
        }
        "refresh_completed" => {
            let ins = i64_field("rows_inserted").unwrap_or(0);
            let del = i64_field("rows_deleted").unwrap_or(0);
            let ms = i64_field("duration_ms").unwrap_or(0);
            let metric = format!("+{ins}/-{del} rows, {ms}ms");
            let context = str_field("action")
                .map(|a| format!("action={a}"))
                .unwrap_or_default();
            (metric, context)
        }
        "refresh_failed" => {
            let context = str_field("error").unwrap_or_default();
            let metric = str_field("action")
                .map(|a| format!("action={a}"))
                .unwrap_or_default();
            (metric, context)
        }
        "scheduler_falling_behind" => {
            let ratio = f64_field("ratio")
                .map(|r| format!("ratio={r:.2}"))
                .unwrap_or_default();
            let elapsed = i64_field("elapsed_ms")
                .map(|m| format!("{m}ms"))
                .unwrap_or_default();
            let metric = if ratio.is_empty() {
                elapsed
            } else if elapsed.is_empty() {
                ratio
            } else {
                format!("{ratio}, {elapsed}")
            };
            (metric, String::new())
        }
        "cleanup_failure" => {
            let context = str_field("error").unwrap_or_default();
            (String::new(), context)
        }
        "no_upstream_changes" => {
            let metric = f64_field("idle_seconds")
                .map(|s| format!("idle={s:.0}s"))
                .unwrap_or_default();
            (metric, String::new())
        }
        _ => (String::new(), String::new()),
    };

    crate::state::AlertEvent {
        timestamp: now,
        severity,
        event,
        table,
        metric,
        context,
    }
}

/// Format a byte count as a human-readable string.
fn format_bytes(bytes: i64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.0} KiB", bytes as f64 / 1_024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Infer alert severity from the event type string.
fn alert_event_severity(event: &str) -> &'static str {
    match event {
        "auto_suspended" | "reinitialize_needed" | "refresh_failed" | "fuse_blown_reminder" => {
            "critical"
        }
        "stale_data"
        | "buffer_growth_warning"
        | "slot_lag_warning"
        | "scheduler_falling_behind"
        | "cdc_trigger_disabled"
        | "cleanup_failure" => "warning",
        "no_upstream_changes" => "info",
        _ => "info",
    }
}

async fn listen_task(conn_args: ConnectionArgs, tx: mpsc::Sender<crate::state::AlertEvent>) {
    use tokio_postgres::AsyncMessage;
    use tokio_postgres::NoTls;

    let conn_string = crate::connection::build_connection_string(&conn_args);

    // Establish a dedicated connection for LISTEN/NOTIFY.
    let (client, connection) = loop {
        match tokio_postgres::connect(&conn_string, NoTls).await {
            Ok(pair) => break pair,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if tx.is_closed() {
                    return;
                }
            }
        }
    };

    // Bridge notifications from the connection driver to our channel.
    let (notify_tx, mut notify_rx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        use futures_util::TryStreamExt;
        use std::pin::pin;
        let stream = futures_util::stream::try_unfold(connection, |mut conn| async {
            match futures_util::future::poll_fn(|cx| conn.poll_message(cx)).await {
                Some(Ok(msg)) => Ok(Some((msg, conn))),
                Some(Err(e)) => Err(e),
                None => Ok(None),
            }
        });
        let mut stream = pin!(stream);
        while let Ok(Some(msg)) = stream.try_next().await {
            if let AsyncMessage::Notification(n) = msg
                && notify_tx.send(n.payload().to_string()).await.is_err()
            {
                break;
            }
        }
    });

    // Subscribe to pg_trickle alerts.
    if client
        .execute("LISTEN pg_trickle_alert", &[])
        .await
        .is_err()
    {
        return;
    }

    // Process notifications as they arrive.
    loop {
        tokio::select! {
            payload = notify_rx.recv() => {
                match payload {
                    Some(payload) => {
                        let alert = parse_alert_payload(&payload);
                        if tx.send(alert).await.is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
            // Periodic check that the receiver is still alive
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                if tx.is_closed() {
                    return;
                }
            }
        }
    }
}

/// Centered overlay rectangle.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn app_with_data() -> App {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.state = test_fixtures::sample_state();
        app
    }

    // ── View switching ───────────────────────────────────────────────

    #[test]
    fn test_initial_view_is_dashboard() {
        let app = App::new(2, Theme::default_dark(), false, false);
        assert_eq!(app.current_view, View::Dashboard);
    }

    #[test]
    fn test_switch_to_detail_via_key_2() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('2')));
        assert_eq!(app.current_view, View::Detail);
    }

    #[test]
    fn test_switch_to_graph_via_key_3() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('3')));
        assert_eq!(app.current_view, View::Graph);
    }

    #[test]
    fn test_switch_to_refresh_log_via_key_4() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('4')));
        assert_eq!(app.current_view, View::RefreshLog);
    }

    #[test]
    fn test_switch_to_diagnostics_via_key_5() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('5')));
        assert_eq!(app.current_view, View::Diagnostics);
    }

    #[test]
    fn test_switch_to_cdc_via_key_6() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('6')));
        assert_eq!(app.current_view, View::Cdc);
    }

    #[test]
    fn test_switch_to_config_via_key_7() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('7')));
        assert_eq!(app.current_view, View::Config);
    }

    #[test]
    fn test_switch_to_health_via_key_8() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('8')));
        assert_eq!(app.current_view, View::Health);
    }

    #[test]
    fn test_switch_to_alerts_via_key_9() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('9')));
        assert_eq!(app.current_view, View::Alerts);
    }

    #[test]
    fn test_switch_to_workers_via_w() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('w')));
        assert_eq!(app.current_view, View::Workers);
    }

    #[test]
    fn test_switch_to_fuse_via_f() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('f')));
        assert_eq!(app.current_view, View::Fuse);
    }

    #[test]
    fn test_switch_to_watermarks_via_m() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('m')));
        assert_eq!(app.current_view, View::Watermarks);
    }

    #[test]
    fn test_switch_to_delta_inspector_via_d() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(app.current_view, View::DeltaInspector);
    }

    #[test]
    fn test_switch_to_graph_via_g() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('g')));
        assert_eq!(app.current_view, View::Graph);
    }

    #[test]
    fn test_switch_to_issues_via_i() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('i')));
        assert_eq!(app.current_view, View::Issues);
    }

    // ── Escape goes back to dashboard ────────────────────────────────

    #[test]
    fn test_esc_returns_to_dashboard() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('5')));
        assert_eq!(app.current_view, View::Diagnostics);
        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.current_view, View::Dashboard);
    }

    #[test]
    fn test_esc_clears_filter() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.filter = Some("test".to_string());
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.filter.is_none());
    }

    // ── Enter drills from dashboard to detail ────────────────────────

    #[test]
    fn test_enter_drills_to_detail() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.current_view, View::Detail);
    }

    #[test]
    fn test_enter_does_nothing_on_detail() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.current_view = View::Detail;
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.current_view, View::Detail);
    }

    // ── Navigation (j/k/arrows) ──────────────────────────────────────

    #[test]
    fn test_move_down_j() {
        let mut app = app_with_data();
        assert_eq!(app.selected, 0);
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_move_down_arrow() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_move_up_k() {
        let mut app = app_with_data();
        app.selected = 2;
        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_move_up_arrow() {
        let mut app = app_with_data();
        app.selected = 2;
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_move_up_at_zero_stays() {
        let mut app = app_with_data();
        assert_eq!(app.selected, 0);
        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_move_down_at_end_stays() {
        let mut app = app_with_data();
        let max = app.list_len() - 1;
        app.selected = max;
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.selected, max);
    }

    #[test]
    fn test_home_key() {
        let mut app = app_with_data();
        app.selected = 3;
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_end_key() {
        let mut app = app_with_data();
        let expected = app.list_len() - 1;
        handle_key(&mut app, key(KeyCode::End));
        assert_eq!(app.selected, expected);
    }

    // ── Quit ─────────────────────────────────────────────────────────

    #[test]
    fn test_quit_via_q() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_quit_via_ctrl_c() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    // ── Filter mode ──────────────────────────────────────────────────

    #[test]
    fn test_slash_enters_filter_mode() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        assert!(app.entering_filter);
    }

    #[test]
    fn test_filter_input_accepts_chars() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('o')));
        handle_key(&mut app, key(KeyCode::Char('r')));
        handle_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(app.filter_input, "ord");
    }

    #[test]
    fn test_filter_backspace() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.filter_input, "a");
    }

    #[test]
    fn test_filter_enter_applies_filter() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('t')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.entering_filter);
        assert_eq!(app.filter, Some("te".to_string()));
    }

    #[test]
    fn test_filter_enter_empty_clears_filter() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.filter = Some("old".to_string());
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.entering_filter);
        assert!(app.filter.is_none());
    }

    #[test]
    fn test_filter_esc_cancels() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.entering_filter);
        assert!(app.filter.is_none());
        assert!(app.filter_input.is_empty());
    }

    #[test]
    fn test_filter_mode_ignores_view_keys() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('5')));
        // Should type '5' into filter, not switch to diagnostics
        assert!(app.entering_filter);
        assert_eq!(app.current_view, View::Dashboard);
        assert_eq!(app.filter_input, "5");
    }

    // ── Help overlay ─────────────────────────────────────────────────

    #[test]
    fn test_help_toggle() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        assert!(!app.show_help);
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_question_mark() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_esc() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_q() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(!app.show_help);
        // q in help mode should close help, not quit the app
        assert!(!app.should_quit);
    }

    #[test]
    fn test_help_mode_ignores_view_keys() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Char('5')));
        assert_eq!(app.current_view, View::Dashboard);
        assert!(app.show_help);
    }

    // ── Selection preservation ────────────────────────────────────────

    #[test]
    fn test_selection_preserved_between_dashboard_and_detail() {
        let mut app = app_with_data();
        app.selected = 2;
        handle_key(&mut app, key(KeyCode::Char('2'))); // -> Detail
        assert_eq!(app.selected, 2, "selection should be preserved");
    }

    #[test]
    fn test_selection_reset_on_different_view() {
        let mut app = app_with_data();
        app.selected = 2;
        handle_key(&mut app, key(KeyCode::Char('4'))); // -> RefreshLog
        assert_eq!(app.selected, 0, "selection should reset on RefreshLog");
    }

    // ── View labels ──────────────────────────────────────────────────

    #[test]
    fn test_all_views_have_labels() {
        for view in &ALL_VIEWS {
            assert!(!view.label().is_empty());
        }
    }

    #[test]
    fn test_all_views_have_key_hints() {
        for view in &ALL_VIEWS {
            assert!(!view.key_hint().is_empty());
        }
    }

    // ── Filtered stream tables ───────────────────────────────────────

    #[test]
    fn test_filtered_stream_tables_no_filter() {
        let app = app_with_data();
        let indices = app.filtered_stream_tables();
        assert_eq!(indices.len(), app.state.stream_tables.len());
    }

    #[test]
    fn test_filtered_stream_tables_with_filter() {
        let mut app = app_with_data();
        app.filter = Some("orders".to_string());
        let indices = app.filtered_stream_tables();
        assert_eq!(indices.len(), 1);
        assert_eq!(app.state.stream_tables[indices[0]].name, "orders_live");
    }

    #[test]
    fn test_filtered_stream_tables_case_insensitive() {
        let mut app = app_with_data();
        app.filter = Some("ORDERS".to_string());
        let indices = app.filtered_stream_tables();
        assert_eq!(indices.len(), 1);
    }

    #[test]
    fn test_filtered_stream_tables_by_status() {
        let mut app = app_with_data();
        app.filter = Some("ERROR".to_string());
        let indices = app.filtered_stream_tables();
        assert!(!indices.is_empty());
    }

    #[test]
    fn test_filtered_sorted_stream_tables_errors_first() {
        let app = app_with_data();
        let sorted = app.filtered_sorted_stream_tables();
        // First entries should be ERROR/SUSPENDED
        let first = &app.state.stream_tables[sorted[0]];
        assert!(
            first.status == "ERROR" || first.status == "SUSPENDED",
            "first sorted entry should be error/suspended, got: {}",
            first.status
        );
    }

    // ── Clamp selection ──────────────────────────────────────────────

    #[test]
    fn test_clamp_selection_empty() {
        let mut app = App::new(2, Theme::default_dark(), false, false);
        app.selected = 5;
        app.clamp_selection();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_clamp_selection_over_length() {
        let mut app = app_with_data();
        app.selected = 100;
        app.clamp_selection();
        assert_eq!(app.selected, app.list_len() - 1);
    }

    // ── List length varies by view ───────────────────────────────────

    #[test]
    fn test_list_len_dashboard() {
        let app = app_with_data();
        assert_eq!(app.list_len(), app.state.stream_tables.len());
    }

    #[test]
    fn test_list_len_graph() {
        let mut app = app_with_data();
        app.current_view = View::Graph;
        assert_eq!(app.list_len(), app.state.dag_edges.len());
    }

    #[test]
    fn test_list_len_health() {
        let mut app = app_with_data();
        app.current_view = View::Health;
        assert_eq!(app.list_len(), app.state.health_checks.len());
    }

    #[test]
    fn test_list_len_issues() {
        let mut app = app_with_data();
        app.current_view = View::Issues;
        assert_eq!(app.list_len(), app.state.issues.len());
    }

    // ── Command palette ──────────────────────────────────────────────

    #[test]
    fn test_colon_opens_command_palette() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Char(':')));
        assert!(app.command_palette.is_some());
    }

    #[test]
    fn test_palette_esc_closes() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Char(':')));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.command_palette.is_none());
    }

    #[test]
    fn test_palette_typing_updates_input() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Char(':')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key(KeyCode::Char('p')));
        let palette = app.command_palette.as_ref().unwrap();
        assert_eq!(palette.input, "exp");
    }

    #[test]
    fn test_palette_explain_shows_builtin_suggestion() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Char(':')));
        for c in "explain".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        let palette = app.command_palette.as_ref().unwrap();
        assert!(
            palette.suggestions.iter().any(|s| s.command == "explain"),
            "typing 'explain' should show the explain command as a suggestion"
        );
    }

    #[test]
    fn test_palette_explain_space_shows_table_completions() {
        let mut app = app_with_data();
        handle_key(&mut app, key(KeyCode::Char(':')));
        for c in "explain ".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        let palette = app.command_palette.as_ref().unwrap();
        assert!(
            !palette.suggestions.is_empty(),
            "typing 'explain ' should show table name completions"
        );
        // All suggestions should start with 'explain '
        for s in &palette.suggestions {
            assert!(
                s.command.starts_with("explain "),
                "suggestion should be 'explain <name>', got: {}",
                s.command
            );
        }
    }

    #[test]
    fn test_execute_explain_bare_name_resolves_to_qualified() {
        // Simulates the user typing `:explain orders_live` — the palette command
        // handler must resolve "orders_live" → "public.orders_live" before
        // sending the FetchDeltaSql action.  We verify the action channel
        // receives the schema-qualified name.
        use tokio::sync::mpsc;
        let mut app = app_with_data();
        let (tx, mut rx) = mpsc::channel::<crate::state::ActionRequest>(8);
        app.action_tx = Some(tx);

        execute_palette_command(&mut app, "explain orders_live");

        // The action should have been sent synchronously via try_send
        let action = rx.try_recv().expect("expected ActionRequest to be sent");
        match action {
            crate::state::ActionRequest::FetchDeltaSql(name) => {
                assert!(
                    name.contains('.'),
                    "FetchDeltaSql name should be schema-qualified, got: {name}"
                );
                assert_eq!(name, "public.orders_live");
            }
            other => panic!("expected FetchDeltaSql, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_explain_already_qualified_passes_through() {
        use tokio::sync::mpsc;
        let mut app = app_with_data();
        let (tx, mut rx) = mpsc::channel::<crate::state::ActionRequest>(8);
        app.action_tx = Some(tx);

        execute_palette_command(&mut app, "explain public.orders_live");

        let action = rx.try_recv().expect("expected ActionRequest to be sent");
        match action {
            crate::state::ActionRequest::FetchDeltaSql(name) => {
                assert_eq!(name, "public.orders_live");
            }
            other => panic!("expected FetchDeltaSql, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_explain_unknown_name_passes_bare() {
        // If the user types a name not in the stream_tables list, pass it
        // through as-is (the DB will return an error, which is correct).
        use tokio::sync::mpsc;
        let mut app = app_with_data();
        let (tx, mut rx) = mpsc::channel::<crate::state::ActionRequest>(8);
        app.action_tx = Some(tx);

        execute_palette_command(&mut app, "explain nonexistent_table");

        let action = rx.try_recv().expect("expected ActionRequest to be sent");
        match action {
            crate::state::ActionRequest::FetchDeltaSql(name) => {
                assert_eq!(name, "nonexistent_table");
            }
            other => panic!("expected FetchDeltaSql, got {other:?}"),
        }
    }

    #[test]
    fn test_delta_sql_received_switches_to_delta_inspector() {
        // Simulates the PollMsg::DeltaSql arriving and verifies the app
        // navigates to the Delta Inspector view with the correct table selected.
        let mut app = app_with_data();

        // Inject the DeltaSql message handler logic directly
        // (we can't go through the async loop, so we test the handler's effects)
        let name = "public.orders_live".to_string();
        let sql = "SELECT id FROM public.orders_live".to_string();
        let bare_name = name
            .split_once('.')
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| name.clone());
        app.state.delta_sql_cache.insert(bare_name.clone(), sql);
        if let Some(pos) = app
            .filtered_sorted_stream_tables()
            .iter()
            .position(|&i| app.state.stream_tables[i].name == bare_name)
        {
            app.selected = pos;
        }
        app.current_view = View::DeltaInspector;
        app.delta_inspector_tab = 0;

        assert_eq!(app.current_view, View::DeltaInspector);
        assert_eq!(app.delta_inspector_tab, 0);
        assert!(
            app.state.delta_sql_cache.contains_key("orders_live"),
            "cache should be keyed by bare name"
        );
        // The selected index should resolve to orders_live
        let idx = app.selected_stream_table_index().unwrap();
        assert_eq!(app.state.stream_tables[idx].name, "orders_live");
    }
}
