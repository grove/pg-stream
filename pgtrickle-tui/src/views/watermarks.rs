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
    active_tab: usize,
) {
    match active_tab {
        1 => render_gates_tab(frame, area, state, theme, selected),
        _ => render_groups_tab(frame, area, state, theme, selected),
    }
}

fn render_groups_tab(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
) {
    let has_alignment = !state.watermark_alignment.is_empty();

    if has_alignment {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        render_groups_table(frame, chunks[0], state, theme, selected);
        render_alignment(frame, chunks[1], state, theme);
    } else {
        render_groups_table(frame, area, state, theme, selected);
    }
}

fn render_groups_table(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
) {
    let header = Row::new(vec!["Group", "Sources", "Tolerance (s)", "Created At"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .watermark_groups
        .iter()
        .enumerate()
        .map(|(i, wg)| {
            let row = Row::new(vec![
                Cell::from(wg.group_name.as_str()),
                Cell::from(format!("{}", wg.source_count)),
                Cell::from(format!("{:.1}", wg.tolerance_secs)),
                Cell::from(wg.created_at.as_deref().unwrap_or("-")),
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
        Constraint::Percentage(12),
        Constraint::Percentage(20),
        Constraint::Percentage(43),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                format!(
                    " [Tab 1] Watermark Groups ({} groups) — Tab to switch ",
                    state.watermark_groups.len()
                ),
                theme.title,
            )),
    );

    frame.render_widget(table, area);
}

fn render_alignment(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let header = Row::new(
        ["Group", "Lag (s)", "Aligned", "Sources", "Coverage"]
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .watermark_alignment
        .iter()
        .map(|wa| {
            let lag_str = wa
                .lag_secs
                .map(|l| format!("{l:.1}"))
                .unwrap_or_else(|| "-".to_string());
            let lag_style = match wa.lag_secs {
                Some(l) if l > 60.0 => theme.error,
                Some(l) if l > 10.0 => theme.warning,
                _ => theme.ok,
            };
            let aligned_icon = if wa.aligned { "✓" } else { "✗" };
            let aligned_style = if wa.aligned { theme.ok } else { theme.warning };
            let coverage = format!("{}/{}", wa.sources_with_watermark, wa.sources_total);
            Row::new(vec![
                Cell::from(wa.group_name.as_str()),
                Cell::from(lag_str).style(lag_style),
                Cell::from(aligned_icon).style(aligned_style),
                Cell::from(format!("{}", wa.sources_total)),
                Cell::from(coverage),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(30),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(12),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Watermark Alignment ", theme.title));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_gates_tab(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
) {
    if state.source_gates.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                " [Tab 2] Source Gates — Tab to switch ",
                theme.title,
            ));
        frame.render_widget(
            Paragraph::new(Line::styled(
                " No source gates configured. Press Tab to return to groups.",
                theme.dim,
            ))
            .block(block),
            area,
        );
        return;
    }

    let header = Row::new(
        [
            "Source",
            "Schema",
            "Gated",
            "Gated At",
            "Duration",
            "Affected STs",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .source_gates
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let gate_style = if g.gated { theme.warning } else { theme.ok };
            let gate_icon = if g.gated { "⚠ Yes" } else { "✓ No" };
            let style = if i == selected {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            Row::new(vec![
                Cell::from(g.source_table.as_str()),
                Cell::from(g.schema_name.as_str()),
                Cell::from(gate_icon).style(gate_style),
                Cell::from(g.gated_at.as_deref().unwrap_or("-")),
                Cell::from(g.gate_duration.as_deref().unwrap_or("-")),
                Cell::from(g.affected_stream_tables.as_deref().unwrap_or("-")),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Min(18),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Min(18),
    ];

    let gated_count = state.source_gates.iter().filter(|g| g.gated).count();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(
                " [Tab 2] Source Gates ({} sources, {} gated) — g to toggle, Tab to switch ",
                state.source_gates.len(),
                gated_count
            ),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}
