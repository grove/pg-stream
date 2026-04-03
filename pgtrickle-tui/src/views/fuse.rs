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
    let has_selection = !state.fuses.is_empty();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_selection {
            vec![Constraint::Percentage(60), Constraint::Percentage(40)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(area);

    render_table(frame, chunks[0], state, theme, selected, filter);

    if has_selection && chunks.len() > 1 {
        render_detail(frame, chunks[1], state, theme, selected);
    }
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
    let header = Row::new(vec![
        "Stream Table",
        "Fuse State",
        "Errors",
        "Last Error",
        "Blown At",
    ])
    .style(theme.header)
    .bottom_margin(1);

    let rows: Vec<Row> = state
        .fuses
        .iter()
        .filter(|fuse| f.is_empty() || fuse.stream_table.to_lowercase().contains(&f))
        .enumerate()
        .map(|(i, fuse)| {
            let state_style = match fuse.fuse_state.as_str() {
                "BLOWN" => theme.error,
                "TRIPPED" => theme.warning,
                "OK" | "HEALTHY" => theme.active,
                _ => theme.dim,
            };
            let icon = match fuse.fuse_state.as_str() {
                "BLOWN" => "✗",
                "TRIPPED" => "⚠",
                _ => "✓",
            };
            let row = Row::new(vec![
                Cell::from(fuse.stream_table.as_str()),
                Cell::from(format!("{icon} {}", fuse.fuse_state)).style(state_style),
                Cell::from(format!("{}", fuse.consecutive_errors)),
                Cell::from(fuse.last_error.as_deref().unwrap_or("-")),
                Cell::from(fuse.blown_at.as_deref().unwrap_or("-")),
            ]);
            if i == selected {
                row.style(theme.selected)
            } else {
                row
            }
        })
        .collect();

    let blown = state
        .fuses
        .iter()
        .filter(|f| f.fuse_state == "BLOWN")
        .count();

    let widths = [
        Constraint::Percentage(22),
        Constraint::Percentage(15),
        Constraint::Percentage(10),
        Constraint::Percentage(33),
        Constraint::Percentage(20),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                format!(
                    " Fuse & Circuit Breaker ({} blown, {} total) ",
                    blown,
                    state.fuses.len()
                ),
                theme.title,
            )),
    );

    frame.render_widget(table, area);
}

fn render_detail(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme, selected: usize) {
    let fuse = match state.fuses.get(selected) {
        Some(f) => f,
        None => return,
    };

    let state_style = match fuse.fuse_state.as_str() {
        "BLOWN" => theme.error,
        "TRIPPED" => theme.warning,
        "OK" | "HEALTHY" => theme.active,
        _ => theme.dim,
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Table: ", theme.header),
            Span::raw(&fuse.stream_table),
        ]),
        Line::from(vec![
            Span::styled(" State: ", theme.header),
            Span::styled(&fuse.fuse_state, state_style),
        ]),
        Line::from(vec![
            Span::styled(" Consecutive Errors: ", theme.header),
            Span::raw(format!("{}", fuse.consecutive_errors)),
        ]),
    ];

    if let Some(ref err) = fuse.last_error {
        lines.push(Line::from(vec![
            Span::styled(" Last Error: ", theme.header),
            Span::styled(err.as_str(), theme.error),
        ]));
    }
    if let Some(ref at) = fuse.blown_at {
        lines.push(Line::from(vec![
            Span::styled(" Blown At: ", theme.header),
            Span::raw(at.as_str()),
        ]));
    }

    if fuse.fuse_state == "BLOWN" {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " To reset: SELECT pgtrickle.reset_fuse('<table>');",
            theme.warning,
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Fuse Detail ", theme.title));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}
