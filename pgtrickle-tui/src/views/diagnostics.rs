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
    // If a row is selected and has signals, show signal breakdown below
    let has_signals = state
        .diagnostics
        .get(selected)
        .and_then(|d| d.signals.as_ref())
        .is_some();

    // UX-7: Show scheduler overhead panel when self-monitoring is active.
    let has_overhead = state.scheduler_overhead.is_some();

    if has_signals && has_overhead {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(8),
                Constraint::Length(12),
                Constraint::Length(5),
            ])
            .split(area);
        render_table(frame, chunks[0], state, theme, selected, filter);
        render_signal_breakdown(frame, chunks[1], state, theme, selected);
        render_scheduler_overhead(frame, chunks[2], state, theme);
    } else if has_signals {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(12)])
            .split(area);
        render_table(frame, chunks[0], state, theme, selected, filter);
        render_signal_breakdown(frame, chunks[1], state, theme, selected);
    } else if has_overhead {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(5)])
            .split(area);
        render_table(frame, chunks[0], state, theme, selected, filter);
        render_scheduler_overhead(frame, chunks[1], state, theme);
    } else {
        render_table(frame, area, state, theme, selected, filter);
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
    let header = Row::new(
        [
            "Schema",
            "Name",
            "Current",
            "Recommended",
            "Confidence",
            "Reason",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .diagnostics
        .iter()
        .filter(|d| {
            f.is_empty()
                || d.name.to_lowercase().contains(&f)
                || d.schema.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, d)| {
            let rec_style = match d.recommended_mode.as_str() {
                "KEEP" => theme.ok,
                _ => theme.warning,
            };
            let style = if i == selected {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            Row::new(vec![
                Cell::from(d.schema.as_str()),
                Cell::from(d.name.as_str()),
                Cell::from(d.current_mode.as_str()),
                Cell::from(d.recommended_mode.as_str()).style(rec_style),
                Cell::from(d.confidence.as_str()),
                Cell::from(d.reason.as_str()),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Min(30),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Diagnostics ({}) ", state.diagnostics.len()),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_signal_breakdown(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
) {
    let diag = match state.diagnostics.get(selected) {
        Some(d) => d,
        None => return,
    };
    let signals = match &diag.signals {
        Some(s) => s,
        None => return,
    };

    let obj = match signals.as_object() {
        Some(o) => o,
        None => return,
    };

    // Collect and sort signals by weight descending
    let mut entries: Vec<(&str, f64, f64)> = obj
        .iter()
        .filter_map(|(key, val)| {
            let score = val.get("score")?.as_f64()?;
            let weight = val.get("weight")?.as_f64()?;
            Some((key.as_str(), score, weight))
        })
        .collect();
    entries.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let bar_max_width = area.width.saturating_sub(35) as f64;
    let mut lines = vec![Line::styled(
        format!(" Signal Breakdown — {} ", diag.name),
        theme.header,
    )];

    for (name, score, weight) in &entries {
        let bar_width = (bar_max_width * score).max(0.0) as usize;
        let bar = "█".repeat(bar_width);
        let bar_style = if *score >= 0.8 {
            theme.active
        } else if *score >= 0.5 {
            theme.ok
        } else {
            theme.warning
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {:<22}", name), theme.dim),
            Span::styled(bar, bar_style),
            Span::styled(
                format!(" {:.0}% (w:{:.0}%)", score * 100.0, weight * 100.0),
                theme.dim,
            ),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Signals ", theme.title));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// UX-7: Render scheduler overhead panel showing self-monitoring cost.
fn render_scheduler_overhead(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let overhead = match &state.scheduler_overhead {
        Some(o) => o,
        None => return,
    };

    let frac_pct = overhead
        .df_refresh_fraction
        .map(|f| format!("{:.1}%", f * 100.0))
        .unwrap_or_else(|| "—".to_string());
    let avg_ms = overhead
        .avg_df_refresh_ms
        .map(|m| format!("{:.1}ms", m))
        .unwrap_or_else(|| "—".to_string());
    let total_s = overhead
        .df_refresh_time_s
        .map(|s| format!("{:.1}s", s))
        .unwrap_or_else(|| "—".to_string());

    let text = format!(
        " Refreshes: {} total, {} DF ({}) │ Avg DF: {} │ DF time: {}",
        overhead.total_refreshes_1h, overhead.df_refreshes_1h, frac_pct, avg_ms, total_s,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Scheduler Overhead (1h) ", theme.title));

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme.dim))).block(block),
        area,
    );
}
