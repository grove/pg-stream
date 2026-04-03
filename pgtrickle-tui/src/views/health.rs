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

    render_overall(frame, chunks[0], state, theme);
    render_checks(frame, chunks[1], state, theme, selected, filter);
}

fn render_overall(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let crit_count = state.critical_health_count();
    let warn_count = state
        .health_checks
        .iter()
        .filter(|h| h.severity == "warning")
        .count();
    let total_st = state.stream_tables.len();
    let errors = state.error_count();

    let (icon, status_text, style) = if crit_count > 0 {
        ("✗", "DEGRADED", theme.error)
    } else if warn_count > 0 || errors > 0 {
        ("⚠", "WARNINGS", theme.warning)
    } else {
        ("●", "HEALTHY", theme.active)
    };

    let line = Line::from(vec![
        Span::styled(format!(" {icon} {status_text} "), style),
        Span::raw("  "),
        Span::styled(format!("{total_st} stream tables"), theme.header),
        Span::raw("  "),
        Span::raw(format!(
            "{} checks ({crit_count} critical, {warn_count} warnings)",
            state.health_checks.len()
        )),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" System Health Overview ", theme.title));

    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_checks(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let f = filter.unwrap_or("").to_lowercase();
    let header = Row::new(vec!["Check", "Severity", "Detail"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .health_checks
        .iter()
        .filter(|h| {
            f.is_empty()
                || h.check_name.to_lowercase().contains(&f)
                || h.detail.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, h)| {
            let sev_style = theme.severity_style(&h.severity);
            let icon = match h.severity.as_str() {
                "critical" => "✗",
                "warning" => "⚠",
                "ok" => "✓",
                _ => "?",
            };
            let row = Row::new(vec![
                Cell::from(h.check_name.as_str()),
                Cell::from(format!("{icon} {}", h.severity)).style(sev_style),
                Cell::from(h.detail.as_str()),
            ]);
            if i == selected {
                row.style(theme.selected)
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Percentage(25),
        Constraint::Percentage(15),
        Constraint::Percentage(60),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" Health Checks ", theme.title)),
    );

    frame.render_widget(table, area);
}
