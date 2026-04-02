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
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::cli::ConnectionArgs;
use crate::error::CliError;
use crate::state::AppState;
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
}

impl App {
    fn new() -> Self {
        Self {
            state: AppState::default(),
            theme: Theme::default_dark(),
            current_view: View::Dashboard,
            selected: 0,
            show_help: false,
            filter: None,
            filter_input: String::new(),
            entering_filter: false,
            should_quit: false,
            force_poll_tx: None,
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

    /// Get filtered stream tables in the same sorted order the Dashboard uses.
    fn filtered_sorted_stream_tables(&self) -> Vec<usize> {
        let mut indices = self.filtered_stream_tables();
        indices.sort_by(|&a, &b| {
            let sa = &self.state.stream_tables[a];
            let sb = &self.state.stream_tables[b];
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
            View::RefreshLog => self.state.refresh_log.len(),
            View::Diagnostics => self.state.diagnostics.len(),
            View::Cdc => self.state.cdc_buffers.len(),
            View::Config => self.state.guc_params.len(),
            View::Health => self.state.health_checks.len(),
            View::Alerts => self.state.alerts.len(),
            View::Workers => self.state.workers.len(),
            View::Fuse => self.state.fuses.len(),
            View::Watermarks => self.state.watermark_groups.len(),
            View::Issues => self.state.issues.len(),
        }
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
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, connection).await;

    // Restore terminal
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    connection: &ConnectionArgs,
) -> Result<(), CliError> {
    let mut app = App::new();

    // Channel for state updates from poller
    let (tx, mut rx) = mpsc::channel::<PollMsg>(4);

    // Channel for force poll requests
    let (force_tx, force_rx) = mpsc::channel::<()>(1);
    app.force_poll_tx = Some(force_tx);

    // Spawn background poller
    let conn_args = connection.clone();
    tokio::spawn(async move {
        poller_task(conn_args, tx, force_rx).await;
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
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            handle_key(&mut app, key);
        }

        // Drain state updates from poller
        while let Ok(msg) = rx.try_recv() {
            match msg {
                PollMsg::StateUpdate(new_state) => {
                    // Preserve alerts from LISTEN/NOTIFY (state polls don't include them)
                    let alerts = std::mem::take(&mut app.state.alerts);
                    app.state = *new_state;
                    app.state.alerts = alerts;
                    app.clamp_selection();
                }
                PollMsg::Error(e) => {
                    app.state.error_message = Some(e);
                    app.state.connected = false;
                }
            }
        }

        // Drain real-time LISTEN/NOTIFY alerts
        while let Ok(alert) = alert_rx.try_recv() {
            app.state.alerts.push(alert);
            // Keep last 200 alerts
            if app.state.alerts.len() > 200 {
                app.state.alerts.remove(0);
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

async fn poller_task(
    conn_args: ConnectionArgs,
    tx: mpsc::Sender<PollMsg>,
    mut force_rx: mpsc::Receiver<()>,
) {
    // Try to connect; retry on failure
    let client = loop {
        match crate::connection::connect(&conn_args).await {
            Ok(c) => break c,
            Err(e) => {
                let _ = tx.send(PollMsg::Error(format!("Connecting: {e}"))).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                if tx.is_closed() {
                    return;
                }
            }
        }
    };

    let mut tick = interval(Duration::from_secs(2));

    loop {
        // Wait for either a tick or a force poll request
        tokio::select! {
            _ = tick.tick() => {}
            _ = force_rx.recv() => {}
        }

        if tx.is_closed() {
            return;
        }

        let mut state = AppState {
            poll_interval_ms: 2000,
            ..AppState::default()
        };
        crate::poller::poll_all(&client, &mut state).await;

        if tx
            .send(PollMsg::StateUpdate(Box::new(state)))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
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
        KeyCode::Char('0') => switch_view(app, View::Health),
        // Extended view switching via letter keys
        KeyCode::Char('w') => switch_view(app, View::Workers),
        KeyCode::Char('f') => switch_view(app, View::Fuse),
        KeyCode::Char('m') => switch_view(app, View::Watermarks),
        KeyCode::Char('d') => switch_view(app, View::DeltaInspector),
        KeyCode::Char('g') => switch_view(app, View::Graph),
        KeyCode::Char('i') => switch_view(app, View::Issues),
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Enter => {
            // Drill from dashboard to detail
            if app.current_view == View::Dashboard {
                app.current_view = View::Detail;
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
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let conn_status = if app.state.connected {
        Span::styled("● connected", app.theme.active)
    } else if app.state.reconnecting {
        Span::styled("◌ reconnecting…", app.theme.warning)
    } else {
        Span::styled("✗ disconnected", app.theme.error)
    };

    let view_label = Span::styled(
        format!(" {} ", app.current_view.label()),
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

    let header = Line::from(vec![
        Span::styled(" pg_trickle ", app.theme.title),
        view_label,
        Span::raw(" "),
        conn_status,
        Span::raw(" "),
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
    match app.current_view {
        View::Dashboard => views::dashboard::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected,
            app.filter.as_deref(),
        ),
        View::Detail => views::detail::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected_stream_table_index(),
        ),
        View::Graph => views::graph::render(frame, area, &app.state, &app.theme, app.selected),
        View::RefreshLog => {
            views::refresh_log::render(frame, area, &app.state, &app.theme, app.selected)
        }
        View::Diagnostics => {
            views::diagnostics::render(frame, area, &app.state, &app.theme, app.selected)
        }
        View::Cdc => views::cdc::render(frame, area, &app.state, &app.theme, app.selected),
        View::Config => views::config::render(frame, area, &app.state, &app.theme, app.selected),
        View::Health => views::health::render(frame, area, &app.state, &app.theme, app.selected),
        View::Alerts => views::alert::render(frame, area, &app.state, &app.theme),
        View::Workers => views::workers::render(frame, area, &app.state, &app.theme, app.selected),
        View::Fuse => views::fuse::render(frame, area, &app.state, &app.theme, app.selected),
        View::Watermarks => {
            views::watermarks::render(frame, area, &app.state, &app.theme, app.selected)
        }
        View::DeltaInspector => views::delta_inspector::render(
            frame,
            area,
            &app.state,
            &app.theme,
            app.selected_stream_table_index(),
        ),
        View::Issues => views::issues::render(frame, area, &app.state, &app.theme, app.selected),
    }
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut spans = vec![];

    if app.entering_filter {
        spans.push(Span::styled(" /", app.theme.title));
        spans.push(Span::raw(&app.filter_input));
        spans.push(Span::styled("█", app.theme.title));
    } else {
        // Context-sensitive key hints
        for view in &ALL_VIEWS {
            if *view == app.current_view {
                spans.push(Span::styled(
                    format!(" [{}]{} ", view.key_hint(), view.label()),
                    app.theme.footer_active,
                ));
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
                        // Parse severity from JSON payload if possible
                        let (severity, message) = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
                            let sev = v.get("severity")
                                .and_then(|s| s.as_str())
                                .unwrap_or("info")
                                .to_string();
                            let msg = v.get("message")
                                .and_then(|s| s.as_str())
                                .or_else(|| v.get("detail").and_then(|s| s.as_str()))
                                .unwrap_or(&payload)
                                .to_string();
                            (sev, msg)
                        } else {
                            ("info".to_string(), payload)
                        };

                        let alert = crate::state::AlertEvent {
                            timestamp: chrono::Utc::now(),
                            severity,
                            message,
                        };
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
        let mut app = App::new();
        app.state = test_fixtures::sample_state();
        app
    }

    // ── View switching ───────────────────────────────────────────────

    #[test]
    fn test_initial_view_is_dashboard() {
        let app = App::new();
        assert_eq!(app.current_view, View::Dashboard);
    }

    #[test]
    fn test_switch_to_detail_via_key_2() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('2')));
        assert_eq!(app.current_view, View::Detail);
    }

    #[test]
    fn test_switch_to_graph_via_key_3() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('3')));
        assert_eq!(app.current_view, View::Graph);
    }

    #[test]
    fn test_switch_to_refresh_log_via_key_4() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('4')));
        assert_eq!(app.current_view, View::RefreshLog);
    }

    #[test]
    fn test_switch_to_diagnostics_via_key_5() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('5')));
        assert_eq!(app.current_view, View::Diagnostics);
    }

    #[test]
    fn test_switch_to_cdc_via_key_6() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('6')));
        assert_eq!(app.current_view, View::Cdc);
    }

    #[test]
    fn test_switch_to_config_via_key_7() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('7')));
        assert_eq!(app.current_view, View::Config);
    }

    #[test]
    fn test_switch_to_health_via_key_8() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('8')));
        assert_eq!(app.current_view, View::Health);
    }

    #[test]
    fn test_switch_to_alerts_via_key_9() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('9')));
        assert_eq!(app.current_view, View::Alerts);
    }

    #[test]
    fn test_switch_to_workers_via_w() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('w')));
        assert_eq!(app.current_view, View::Workers);
    }

    #[test]
    fn test_switch_to_fuse_via_f() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('f')));
        assert_eq!(app.current_view, View::Fuse);
    }

    #[test]
    fn test_switch_to_watermarks_via_m() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('m')));
        assert_eq!(app.current_view, View::Watermarks);
    }

    #[test]
    fn test_switch_to_delta_inspector_via_d() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(app.current_view, View::DeltaInspector);
    }

    #[test]
    fn test_switch_to_graph_via_g() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('g')));
        assert_eq!(app.current_view, View::Graph);
    }

    #[test]
    fn test_switch_to_issues_via_i() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('i')));
        assert_eq!(app.current_view, View::Issues);
    }

    // ── Escape goes back to dashboard ────────────────────────────────

    #[test]
    fn test_esc_returns_to_dashboard() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('5')));
        assert_eq!(app.current_view, View::Diagnostics);
        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.current_view, View::Dashboard);
    }

    #[test]
    fn test_esc_clears_filter() {
        let mut app = App::new();
        app.filter = Some("test".to_string());
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.filter.is_none());
    }

    // ── Enter drills from dashboard to detail ────────────────────────

    #[test]
    fn test_enter_drills_to_detail() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.current_view, View::Detail);
    }

    #[test]
    fn test_enter_does_nothing_on_detail() {
        let mut app = App::new();
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
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_quit_via_ctrl_c() {
        let mut app = App::new();
        handle_key(&mut app, ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    // ── Filter mode ──────────────────────────────────────────────────

    #[test]
    fn test_slash_enters_filter_mode() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('/')));
        assert!(app.entering_filter);
    }

    #[test]
    fn test_filter_input_accepts_chars() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('o')));
        handle_key(&mut app, key(KeyCode::Char('r')));
        handle_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(app.filter_input, "ord");
    }

    #[test]
    fn test_filter_backspace() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.filter_input, "a");
    }

    #[test]
    fn test_filter_enter_applies_filter() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('t')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.entering_filter);
        assert_eq!(app.filter, Some("te".to_string()));
    }

    #[test]
    fn test_filter_enter_empty_clears_filter() {
        let mut app = App::new();
        app.filter = Some("old".to_string());
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.entering_filter);
        assert!(app.filter.is_none());
    }

    #[test]
    fn test_filter_esc_cancels() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.entering_filter);
        assert!(app.filter.is_none());
        assert!(app.filter_input.is_empty());
    }

    #[test]
    fn test_filter_mode_ignores_view_keys() {
        let mut app = App::new();
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
        let mut app = App::new();
        assert!(!app.show_help);
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_question_mark() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_esc() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_dismiss_via_q() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(!app.show_help);
        // q in help mode should close help, not quit the app
        assert!(!app.should_quit);
    }

    #[test]
    fn test_help_mode_ignores_view_keys() {
        let mut app = App::new();
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
        assert!(indices.len() >= 1);
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
        let mut app = App::new();
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
}
