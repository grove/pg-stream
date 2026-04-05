use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::state::AppState;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Alerts ({}) ", state.alerts.len()),
            theme.title,
        ));

    if state.alerts.is_empty() {
        let paragraph =
            Paragraph::new(Line::styled(" No alerts", theme.dim)).block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let header = Row::new(vec!["Sev", "Time", "Event", "Table", "Detail"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .alerts
        .iter()
        .map(|alert| {
            let sev_style = theme.severity_style(&alert.severity);
            let icon = match alert.severity.as_str() {
                "critical" => "✗",
                "warning" => "⚠",
                _ => "●",
            };
            Row::new(vec![
                Cell::from(icon).style(sev_style),
                Cell::from(alert.timestamp.format("%H:%M:%S").to_string())
                    .style(theme.dim),
                Cell::from(alert.event.as_str()),
                Cell::from(alert.table.as_str()).style(theme.dim),
                Cell::from(alert.detail.as_str()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Length(10),
        Constraint::Length(24),
        Constraint::Length(24),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}
