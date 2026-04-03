use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

use crate::state::AppState;
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: Option<usize>,
    active_tab: usize,
) {
    // Show delta SQL for the selected stream table (if any).
    let st = selected.and_then(|idx| state.stream_tables.get(idx));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    // Header: which table is selected + tab indicator
    let tab_labels = ["Delta SQL", "Auxiliary Columns"];
    let header_text = match st {
        Some(st) => format!(" Delta Inspector — {}.{} ", st.schema, st.name),
        None => " Delta Inspector — select a stream table on Dashboard ".to_string(),
    };

    let mut header_spans = vec![Span::styled(header_text, theme.title)];
    header_spans.push(Span::raw(" "));
    for (i, label) in tab_labels.iter().enumerate() {
        if i == active_tab {
            header_spans.push(Span::styled(format!(" [{label}] "), theme.footer_active));
        } else {
            header_spans.push(Span::styled(format!(" {label} "), theme.dim));
        }
    }
    header_spans.push(Span::styled(" Tab to switch", theme.dim));

    let header = Paragraph::new(Line::from(header_spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border),
    );
    frame.render_widget(header, chunks[0]);

    match active_tab {
        0 => render_delta_sql(frame, chunks[1], state, theme, st),
        1 => render_auxiliary_columns(frame, chunks[1], state, theme, st),
        _ => {}
    }
}

fn render_delta_sql(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    st: Option<&crate::state::StreamTableInfo>,
) {
    // Body: show cached delta SQL or instructions to fetch
    let body_lines = if let Some(st) = st {
        if let Some(sql) = state.delta_sql_cache.get(&st.name) {
            // Render the actual delta SQL
            let mut lines = vec![
                Line::styled(" Generated Delta SQL (Enter to reload):", theme.header),
                Line::raw(""),
            ];
            for sql_line in sql.lines() {
                lines.push(Line::raw(format!("  {sql_line}")));
            }
            lines
        } else {
            // SQL not yet fetched — show loading state
            vec![
                Line::raw(""),
                Line::styled(" Fetching delta SQL…", theme.dim),
                Line::raw(""),
                Line::styled(" CLI reference:", theme.header),
                Line::raw(""),
                Line::raw(format!("   pgtrickle explain {}", st.name)),
                Line::raw("   pgtrickle explain --analyze"),
                Line::raw("   pgtrickle explain --operators"),
                Line::raw("   pgtrickle explain --dedup"),
            ]
        }
    } else {
        vec![
            Line::raw(""),
            Line::styled(
                " No stream table selected. Press 1 to go to Dashboard, select a table, then press d.",
                theme.dim,
            ),
        ]
    };

    let body = Paragraph::new(body_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(" Delta SQL  e-Show DDL ", theme.title)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(body, area);
}

fn render_auxiliary_columns(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    st: Option<&crate::state::StreamTableInfo>,
) {
    let st = match st {
        Some(st) => st,
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(" Auxiliary Columns ", theme.title));
            frame.render_widget(
                Paragraph::new(Line::styled(" No stream table selected", theme.dim)).block(block),
                area,
            );
            return;
        }
    };

    let cols = state.auxiliary_columns_cache.get(&st.name);

    match cols {
        Some(cols) if !cols.is_empty() => {
            let header = Row::new(
                ["Column", "Type", "Purpose"]
                    .iter()
                    .map(|h| Cell::from(*h).style(theme.header)),
            )
            .height(1);

            let rows: Vec<Row> = cols
                .iter()
                .map(|c| {
                    Row::new(vec![
                        Cell::from(c.column_name.as_str()),
                        Cell::from(c.data_type.as_str()),
                        Cell::from(c.purpose.as_str()),
                    ])
                })
                .collect();

            let widths = [
                Constraint::Min(25),
                Constraint::Length(16),
                Constraint::Min(30),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(
                    format!(" Auxiliary Columns: {} ({}) ", st.name, cols.len()),
                    theme.title,
                ));

            let table = Table::new(rows, widths).header(header).block(block);
            frame.render_widget(table, area);
        }
        Some(_) => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(
                    format!(" Auxiliary Columns: {} ", st.name),
                    theme.title,
                ));
            frame.render_widget(
                Paragraph::new(Line::styled(" No auxiliary columns found", theme.dim)).block(block),
                area,
            );
        }
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border)
                .title(Span::styled(
                    format!(" Auxiliary Columns: {} ", st.name),
                    theme.title,
                ));
            frame.render_widget(
                Paragraph::new(Line::styled(
                    " Loading... (press Tab to switch tabs)",
                    theme.dim,
                ))
                .block(block),
                area,
            );
        }
    }
}
