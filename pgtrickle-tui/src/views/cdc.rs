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
    // Decide layout based on whether we have CDC health or dedup data
    let has_health = !state.cdc_health.is_empty();
    let has_dedup = state.dedup_stats.is_some();
    let has_sbs = !state.shared_buffer_stats.is_empty();

    if has_health || has_dedup || has_sbs {
        // Multi-section layout
        let mut constraints = vec![Constraint::Percentage(30)]; // Buffers
        if has_health {
            constraints.push(Constraint::Percentage(25)); // CDC health
        }
        if has_sbs {
            constraints.push(Constraint::Percentage(25)); // Shared buffer stats
        }
        constraints.push(Constraint::Percentage(20)); // Dedup + triggers

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut idx = 0;
        render_buffers(frame, chunks[idx], state, theme, selected, filter);
        idx += 1;
        if has_health {
            render_cdc_health(frame, chunks[idx], state, theme);
            idx += 1;
        }
        if has_sbs {
            render_shared_buffer_stats(frame, chunks[idx], state, theme);
            idx += 1;
        }

        // Bottom: split horizontally for dedup stats (left) and triggers (right)
        if has_dedup {
            let bottom = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(chunks[idx]);
            render_dedup_stats(frame, bottom[0], state, theme);
            render_triggers(frame, bottom[1], state, theme);
        } else {
            render_triggers(frame, chunks[idx], state, theme);
        }
    } else {
        // Original 2-section layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);

        render_buffers(frame, chunks[0], state, theme, selected, filter);
        render_triggers(frame, chunks[1], state, theme);
    }
}

fn render_buffers(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let f = filter.unwrap_or("").to_lowercase();
    let header = Row::new(
        [
            "Stream Table",
            "Source",
            "CDC Mode",
            "Pending Rows",
            "Buffer Size",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .cdc_buffers
        .iter()
        .filter(|buf| {
            f.is_empty()
                || buf.stream_table.to_lowercase().contains(&f)
                || buf.source_table.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, buf)| {
            let size_style = if buf.buffer_bytes > 10_000_000 {
                theme.error
            } else if buf.buffer_bytes > 1_000_000 {
                theme.warning
            } else {
                theme.ok
            };

            let style = if i == selected {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };

            Row::new(vec![
                Cell::from(buf.stream_table.as_str()),
                Cell::from(buf.source_table.as_str()),
                Cell::from(buf.cdc_mode.as_str()),
                Cell::from(buf.pending_rows.to_string()),
                Cell::from(format_bytes(buf.buffer_bytes)).style(size_style),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(14),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Change Buffers ({}) ", state.cdc_buffers.len()),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_triggers(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let header = Row::new(
        ["Source Table", "Trigger Name", "Events"]
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .trigger_inventory
        .iter()
        .map(|t| {
            Row::new(vec![
                Cell::from(t.source_table.as_str()),
                Cell::from(t.trigger_name.as_str()),
                Cell::from(t.firing_events.as_str()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(35),
        Constraint::Percentage(40),
        Constraint::Percentage(25),
    ];

    let ok_count = state.trigger_inventory.len();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Trigger Inventory ({ok_count} triggers) "),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn format_bytes(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn render_cdc_health(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if state.cdc_health.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" CDC Health ", theme.title));
        frame.render_widget(
            Paragraph::new(Line::styled(" No CDC health data", theme.dim)).block(block),
            area,
        );
        return;
    }

    let header = Row::new(
        ["Source", "Mode", "Slot", "Lag", "LSN", "Alert"]
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .cdc_health
        .iter()
        .map(|h| {
            let lag_str = h
                .lag_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_string());
            let lag_style = match h.lag_bytes {
                Some(b) if b > 10_000_000 => theme.error,
                Some(b) if b > 1_000_000 => theme.warning,
                _ => theme.ok,
            };
            let alert_style = if h.alert.is_some() {
                theme.warning
            } else {
                theme.dim
            };
            Row::new(vec![
                Cell::from(h.source_table.as_str()),
                Cell::from(h.cdc_mode.as_str()),
                Cell::from(h.slot_name.as_deref().unwrap_or("-")),
                Cell::from(lag_str).style(lag_style),
                Cell::from(h.confirmed_lsn.as_deref().unwrap_or("-")),
                Cell::from(h.alert.as_deref().unwrap_or("-")).style(alert_style),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(18),
        Constraint::Length(12),
        Constraint::Min(20),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(10),
    ];

    let alerts = state
        .cdc_health
        .iter()
        .filter(|h| h.alert.is_some())
        .count();
    let title = if alerts > 0 {
        format!(
            " CDC Health ({} sources, {} alerts) ",
            state.cdc_health.len(),
            alerts
        )
    } else {
        format!(" CDC Health ({} sources) ", state.cdc_health.len())
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_dedup_stats(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let ds = match &state.dedup_stats {
        Some(ds) => ds,
        None => return,
    };

    let ratio_style = if ds.dedup_ratio_pct >= 10.0 {
        theme.error
    } else if ds.dedup_ratio_pct >= 5.0 {
        theme.warning
    } else {
        theme.ok
    };

    // Bar chart for ratio
    let bar_width = (area.width.saturating_sub(6) as f64 * ds.dedup_ratio_pct / 100.0)
        .min(area.width.saturating_sub(6) as f64) as usize;
    let bar = "█".repeat(bar_width);

    let lines = vec![
        Line::from(vec![
            Span::styled(" Total DIFF:  ", theme.header),
            Span::raw(ds.total_diff_refreshes.to_string()),
        ]),
        Line::from(vec![
            Span::styled(" Dedup needed:", theme.header),
            Span::raw(format!(" {}", ds.dedup_needed)),
        ]),
        Line::from(vec![
            Span::styled(" Dedup ratio: ", theme.header),
            Span::styled(format!("{:.1}%", ds.dedup_ratio_pct), ratio_style),
        ]),
        Line::raw(""),
        Line::from(vec![Span::raw("  "), Span::styled(bar, ratio_style)]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Dedup Stats ", theme.title));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_shared_buffer_stats(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let header = Row::new(
        [
            "Source",
            "Consumers",
            "Cols",
            "Frontier LSN",
            "Buf Rows",
            "Part",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .shared_buffer_stats
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(s.source_table.as_str()),
                Cell::from(format!("{} ({})", s.consumer_count, s.consumers)),
                Cell::from(s.columns_tracked.to_string()),
                Cell::from(s.safe_frontier_lsn.as_deref().unwrap_or("-")),
                Cell::from(s.buffer_rows.to_string()),
                Cell::from(if s.is_partitioned { "yes" } else { "no" }),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(18),
        Constraint::Min(25),
        Constraint::Length(6),
        Constraint::Length(16),
        Constraint::Length(10),
        Constraint::Length(6),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(
                " Shared Buffer Stats ({}) ",
                state.shared_buffer_stats.len()
            ),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}
