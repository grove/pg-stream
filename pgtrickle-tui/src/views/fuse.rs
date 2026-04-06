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
        "Mode",
        "Fuse State",
        "Ceiling",
        "Blown At",
        "Reason",
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
                "armed" | "OK" | "HEALTHY" => theme.active,
                _ => theme.dim,
            };
            let icon = match fuse.fuse_state.as_str() {
                "BLOWN" => "✗",
                "TRIPPED" => "⚠",
                _ => "✓",
            };
            let ceiling_str = fuse
                .fuse_ceiling
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            let row = Row::new(vec![
                Cell::from(fuse.stream_table.as_str()),
                Cell::from(fuse.fuse_mode.as_str()),
                Cell::from(format!("{icon} {}", fuse.fuse_state)).style(state_style),
                Cell::from(ceiling_str),
                Cell::from(fuse.blown_at.as_deref().unwrap_or("-")),
                Cell::from(fuse.blow_reason.as_deref().unwrap_or("-")),
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
        Constraint::Percentage(18),
        Constraint::Percentage(8),
        Constraint::Percentage(14),
        Constraint::Percentage(8),
        Constraint::Percentage(20),
        Constraint::Percentage(32),
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
            Span::styled(" Mode: ", theme.header),
            Span::raw(&fuse.fuse_mode),
        ]),
        Line::from(vec![
            Span::styled(" State: ", theme.header),
            Span::styled(&fuse.fuse_state, state_style),
        ]),
    ];

    if let Some(ceiling) = fuse.fuse_ceiling {
        lines.push(Line::from(vec![
            Span::styled(" Ceiling: ", theme.header),
            Span::raw(format!("{ceiling}")),
        ]));
    }
    if let Some(ref at) = fuse.blown_at {
        lines.push(Line::from(vec![
            Span::styled(" Blown At: ", theme.header),
            Span::raw(at.as_str()),
        ]));
    }
    if let Some(ref reason) = fuse.blow_reason {
        lines.push(Line::from(vec![
            Span::styled(" Blow Reason: ", theme.header),
            Span::styled(reason.as_str(), theme.error),
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
