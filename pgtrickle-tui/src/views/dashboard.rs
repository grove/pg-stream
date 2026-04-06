use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Sparkline, Table};

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
    let wide = area.width >= 140;
    let tall = area.height >= 35;

    if wide && tall {
        render_wide(frame, area, state, theme, selected, filter);
    } else {
        render_standard(frame, area, state, theme, selected, filter);
    }
}

/// Get filtered and sorted stream table indices.
fn filtered_sorted_indices(state: &AppState, filter: Option<&str>) -> Vec<usize> {
    let f = filter.unwrap_or("").to_lowercase();
    let mut indices: Vec<usize> = state
        .stream_tables
        .iter()
        .enumerate()
        .filter(|(_, st)| {
            if f.is_empty() {
                return true;
            }
            st.name.to_lowercase().contains(&f)
                || st.schema.to_lowercase().contains(&f)
                || st.status.to_lowercase().contains(&f)
                || st.refresh_mode.to_lowercase().contains(&f)
        })
        .map(|(i, _)| i)
        .collect();

    // Default sort: errors first, then cascade-stale, then stale, then by name
    indices.sort_by(|&a, &b| {
        let sa = &state.stream_tables[a];
        let sb = &state.stream_tables[b];
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

fn render_wide(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Status ribbon
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // DAG mini-map
        ])
        .split(area);

    render_status_ribbon(frame, chunks[0], state, theme);

    // Split main into table + issues sidebar.
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(60), Constraint::Length(35)])
        .split(chunks[1]);

    render_table(frame, main[0], state, theme, selected, true, filter);
    render_issues_sidebar(frame, main[1], state, theme);
    render_dag_minimap(frame, chunks[2], state, theme);
}

fn render_standard(
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

    render_status_ribbon(frame, chunks[0], state, theme);
    render_table(frame, chunks[1], state, theme, selected, false, filter);
}

fn render_status_ribbon(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let total = state.stream_tables.len();
    let active = state.active_count();
    let errors = state.error_count();
    let stale = state.stale_count();
    let cascade = state.cascade_stale_count();
    let critical = state.critical_health_count();

    let mut spans = vec![
        Span::styled(format!(" {total} total"), theme.header),
        Span::raw("  "),
        Span::styled(format!("● {active} active"), theme.active),
    ];

    if errors > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("✗ {errors} error"), theme.error));
    }
    if cascade > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("⚠ {cascade} cascade-stale"),
            theme.warning,
        ));
    }
    if stale > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("⚠ {stale} stale"), theme.warning));
    }
    if critical > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("✗ {critical} critical"), theme.error));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Stream Tables ", theme.title));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn render_table(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    show_eff: bool,
    filter: Option<&str>,
) {
    let indices = filtered_sorted_indices(state, filter);

    let mut header_cells = vec![
        "Name",
        "Schema",
        "Status",
        "Mode",
        "Effective",
        "Stale",
        "Last Refresh",
    ];
    if show_eff {
        header_cells.push("Avg ms");
        header_cells.push("Refreshes");
    }

    let header = Row::new(
        header_cells
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = indices
        .iter()
        .enumerate()
        .map(|(display_idx, &real_idx)| {
            let st = &state.stream_tables[real_idx];
            let status_style = theme.status_style(&st.status);
            let stale_str = if st.stale { "yes" } else { "no" };
            let stale_style = if st.stale { theme.warning } else { theme.ok };

            // EFF column (F21): effective staleness considering cascade,
            // plus downgrade hint from explain_mode_cache when available.
            let (eff_str, eff_style) = if st.status == "ERROR" || st.status == "SUSPENDED" {
                ("✗ err".to_string(), theme.error)
            } else if st.cascade_stale {
                ("⚠ cascade".to_string(), theme.warning)
            } else if let Some(explain) = state.explain_mode_cache.get(&st.name) {
                if explain.configured_mode != explain.effective_mode {
                    (format!("{} ↓", explain.effective_mode), theme.warning)
                } else {
                    ("✓ ok".to_string(), theme.ok)
                }
            } else {
                ("✓ ok".to_string(), theme.ok)
            };

            let mut cells = vec![
                Cell::from(st.name.as_str()),
                Cell::from(st.schema.as_str()),
                Cell::from(st.status.as_str()).style(status_style),
                Cell::from(st.refresh_mode.as_str()),
                Cell::from(eff_str).style(eff_style),
                Cell::from(stale_str).style(stale_style),
                Cell::from(st.last_refresh_at.as_deref().unwrap_or("-").to_string()),
            ];

            if show_eff {
                cells.push(Cell::from(
                    st.avg_duration_ms
                        .map(|ms| format!("{ms:.1}"))
                        .unwrap_or_default(),
                ));
                cells.push(Cell::from(st.total_refreshes.to_string()));
            }

            let style = if display_idx == selected {
                theme.selected
            } else {
                Style::default()
            };
            Row::new(cells).style(style)
        })
        .collect();

    let widths = if show_eff {
        vec![
            Constraint::Fill(3),    // Name
            Constraint::Fill(2),    // Schema
            Constraint::Length(11), // Status  (INITIALIZING = 11)
            Constraint::Length(12), // Mode    (DIFFERENTIAL = 12)
            Constraint::Fill(2),    // Effective
            Constraint::Length(5),  // Stale
            Constraint::Fill(2),    // Last Refresh
            Constraint::Length(8),  // Avg ms
            Constraint::Length(10), // Refreshes
        ]
    } else {
        vec![
            Constraint::Fill(3),    // Name
            Constraint::Fill(2),    // Schema
            Constraint::Length(11), // Status
            Constraint::Length(12), // Mode
            Constraint::Fill(2),    // Effective
            Constraint::Length(5),  // Stale
            Constraint::Fill(2),    // Last Refresh
        ]
    };

    let filter_info = filter
        .map(|f| format!(" (filter: {f})"))
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(
                " {} of {} tables{filter_info} ",
                indices.len(),
                state.stream_tables.len()
            ),
            theme.title,
        ));

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(theme.selected)
        .highlight_symbol("▸ ");

    frame.render_widget(table, area);

    // Render sparklines below the table if space permits and in wide mode
    if show_eff && area.height > 20 {
        render_sparklines(frame, area, state, theme, &indices, selected);
    }
}

fn render_sparklines(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    indices: &[usize],
    selected: usize,
) {
    // Show sparkline for the selected ST in a small area at the bottom
    if let Some(&real_idx) = indices.get(selected) {
        let st = &state.stream_tables[real_idx];
        if let Some(data) = state.sparkline_data.get(&st.name)
            && !data.is_empty()
        {
            let spark_data: Vec<u64> = data.iter().map(|&v| v.max(0.0) as u64).collect();
            let max_val = spark_data.iter().copied().max().unwrap_or(1);
            let sparkline = Sparkline::default()
                .data(&spark_data)
                .max(max_val)
                .style(theme.active);
            // Render in a tiny area overlaid at bottom-right
            let spark_area = Rect {
                x: area.x + area.width.saturating_sub(22),
                y: area.y + area.height.saturating_sub(2),
                width: 20.min(area.width),
                height: 1,
            };
            if spark_area.y >= area.y + 3 {
                frame.render_widget(sparkline, spark_area);
            }
        }
    }
}

fn render_issues_sidebar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();

    // Error chain issues
    let errors: Vec<_> = state
        .stream_tables
        .iter()
        .filter(|st| st.status == "ERROR" || st.status == "SUSPENDED")
        .collect();

    for st in &errors {
        lines.push(Line::from(vec![
            Span::styled("✗ ", theme.error),
            Span::styled(&st.name, Style::default().add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(st.status.as_str(), theme.status_style(&st.status)),
        ]));
        if let Some(ref err) = st.last_error_message {
            let truncated = if err.len() > 30 { &err[..30] } else { err };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(truncated, theme.dim),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Buffer growth warnings
    let big_buffers: Vec<_> = state
        .cdc_buffers
        .iter()
        .filter(|b| b.buffer_bytes > 1_000_000)
        .collect();
    for buf in &big_buffers {
        lines.push(Line::from(vec![
            Span::styled("⚠ ", theme.warning),
            Span::raw(format!(
                "Buffer: {} ({} rows)",
                buf.source_table, buf.pending_rows
            )),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::styled("No issues detected", theme.ok));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Issues ({}) ", errors.len() + big_buffers.len()),
            theme.title,
        ));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_dag_minimap(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let lines: Vec<Line> = state
        .dag_edges
        .iter()
        .filter(|e| e.depth <= 2)
        .take(area.height.saturating_sub(2) as usize)
        .map(|e| {
            let status_style = e
                .status
                .as_deref()
                .map(|s| theme.status_style(s))
                .unwrap_or_default();
            Line::from(Span::styled(e.tree_line.as_str(), status_style))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" DAG Mini-Map ", theme.title));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}
