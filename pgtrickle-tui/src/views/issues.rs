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
    if state.issues.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" Issues ", theme.title));

        let msg = Paragraph::new(Line::styled(" ✓ No issues detected", theme.active)).block(block);

        frame.render_widget(msg, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    render_summary(frame, chunks[0], state, theme);
    render_table(frame, chunks[1], state, theme, selected, filter);
}

fn render_summary(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let critical = state
        .issues
        .iter()
        .filter(|i| i.severity == "critical")
        .count();
    let warning = state
        .issues
        .iter()
        .filter(|i| i.severity == "warning")
        .count();
    let info = state.issues.iter().filter(|i| i.severity == "info").count();

    let line = Line::from(vec![
        Span::styled(
            format!(" {critical} critical "),
            if critical > 0 { theme.error } else { theme.dim },
        ),
        Span::raw(" "),
        Span::styled(
            format!("{warning} warnings "),
            if warning > 0 {
                theme.warning
            } else {
                theme.dim
            },
        ),
        Span::raw(" "),
        Span::styled(format!("{info} info "), theme.dim),
        Span::raw(" "),
        Span::styled(format!("{} total issues", state.issues.len()), theme.header),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Issue Summary ", theme.title));

    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_table(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let f = filter.unwrap_or("").to_lowercase();
    let header = Row::new(vec!["Sev", "Category", "Table", "Summary", "Blast"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .issues
        .iter()
        .filter(|issue| {
            f.is_empty()
                || issue
                    .affected_table
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&f)
                || issue.category.to_lowercase().contains(&f)
                || issue.summary.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, issue)| {
            let (icon, sev_style) = match issue.severity.as_str() {
                "critical" | "error" => ("✗", theme.error),
                "warning" => ("⚠", theme.warning),
                _ => ("ℹ", theme.dim),
            };
            let table_cell = match issue.affected_table.as_deref() {
                Some(t) => t.to_string(),
                None => "(system-wide)".to_string(),
            };
            let row = Row::new(vec![
                Cell::from(format!("{icon} {}", issue.severity)).style(sev_style),
                Cell::from(issue.category.as_str()),
                Cell::from(table_cell),
                Cell::from(issue.summary.as_str()),
                Cell::from(format!("{}", issue.blast_radius)),
            ]);
            if i == selected {
                row.style(theme.selected)
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Percentage(12),
        Constraint::Percentage(15),
        Constraint::Percentage(20),
        Constraint::Percentage(43),
        Constraint::Percentage(10),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                format!(" DAG Issues ({}) ", state.issues.len()),
                theme.title,
            )),
    );

    frame.render_widget(table, area);
}
