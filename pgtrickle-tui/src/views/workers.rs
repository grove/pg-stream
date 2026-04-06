use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::state::AppState;
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    render_pool_summary(frame, chunks[0], state, theme);
    render_job_table(frame, chunks[1], state, theme, selected, filter);
}

fn render_pool_summary(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let line = if let Some(ref pool) = state.workers {
        let mode_style = if pool.parallel_mode == "on" {
            theme.ok
        } else {
            theme.warning
        };
        Line::from(vec![
            Span::styled(" Active workers: ", theme.header),
            Span::styled(
                format!("{}/{}", pool.active_workers, pool.max_workers),
                if pool.active_workers > 0 {
                    theme.active
                } else {
                    theme.dim
                },
            ),
            Span::raw("   "),
            Span::styled("Per-DB cap: ", theme.header),
            Span::raw(format!("{}", pool.per_db_cap)),
            Span::raw("   "),
            Span::styled("Parallel mode: ", theme.header),
            Span::styled(pool.parallel_mode.as_str(), mode_style),
        ])
    } else {
        Line::styled(" No pool data", theme.dim)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Worker Pool ", theme.title));

    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_job_table(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let f = filter.unwrap_or("").to_lowercase();

    let header = Row::new(vec![
        "Job ID",
        "Unit Key",
        "Kind",
        "Status",
        "Members",
        "Worker PID",
        "Started",
        "Duration",
    ])
    .style(theme.header)
    .bottom_margin(1);

    let rows: Vec<Row> = state
        .job_queue
        .iter()
        .filter(|j| {
            f.is_empty()
                || j.unit_key.to_lowercase().contains(&f)
                || j.status.to_lowercase().contains(&f)
                || j.unit_kind.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, j)| {
            let status_style = match j.status.as_str() {
                "RUNNING" | "running" => theme.active,
                "SUCCEEDED" | "succeeded" => theme.ok,
                "FAILED" | "failed" => theme.error,
                _ => theme.dim,
            };
            let row = Row::new(vec![
                Cell::from(format!("{}", j.job_id)),
                Cell::from(j.unit_key.as_str()),
                Cell::from(j.unit_kind.as_str()),
                Cell::from(j.status.as_str()).style(status_style),
                Cell::from(format!("{}", j.member_count)),
                Cell::from(
                    j.worker_pid
                        .map(|p| format!("{p}"))
                        .unwrap_or_else(|| "-".to_string()),
                ),
                Cell::from(j.started_at.as_deref().unwrap_or("-")),
                Cell::from(
                    j.duration_ms
                        .map(|d| format!("{d:.1}ms"))
                        .unwrap_or_default(),
                ),
            ]);
            if i == selected {
                row.style(theme.selected)
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Length(9),  // Job ID
        Constraint::Fill(2),    // Unit Key
        Constraint::Length(12), // Kind
        Constraint::Length(10), // Status
        Constraint::Length(8),  // Members
        Constraint::Length(11), // Worker PID
        Constraint::Fill(2),    // Started
        Constraint::Length(12), // Duration
    ];

    let running = state
        .job_queue
        .iter()
        .filter(|j| j.status.to_uppercase() == "RUNNING")
        .count();

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(
                    format!(
                        " Parallel Jobs ({} running / {} total) ",
                        running,
                        state.job_queue.len()
                    ),
                    theme.title,
                )),
        )
        .row_highlight_style(theme.selected);

    frame.render_widget(table, area);
}
