use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

use crate::state::{AppState, StreamTableInfo};
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    st_index: Option<usize>,
) {
    let st = match st_index.and_then(|idx| state.stream_tables.get(idx)) {
        Some(st) => st,
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Detail — no stream table selected ");
            frame.render_widget(block, area);
            return;
        }
    };

    let has_sources = state.source_detail_cache.contains_key(&st.name);
    let has_history = state.refresh_history_cache.contains_key(&st.name);
    let has_errors = state.diagnosed_errors.contains_key(&st.name)
        && !state.diagnosed_errors[&st.name].is_empty();
    let has_cdc_health = state.cdc_health.iter().any(|h| {
        state
            .cdc_buffers
            .iter()
            .any(|b| b.stream_table == st.name && b.source_table == h.source_table)
    });

    // Build constraints based on available data
    let mut constraints = vec![Constraint::Length(12)]; // Properties (expanded for explain mode)
    if has_sources {
        constraints.push(Constraint::Length(8)); // Sources
    }
    constraints.push(Constraint::Length(10)); // Stats + efficiency
    if has_history {
        constraints.push(Constraint::Length(10)); // Rich refresh history
    } else {
        constraints.push(Constraint::Length(8)); // Basic recent refreshes
    }
    if has_cdc_health {
        constraints.push(Constraint::Length(6)); // CDC source health
    }
    if has_errors {
        constraints.push(Constraint::Length(10)); // Error diagnosis
    }
    constraints.push(Constraint::Min(4)); // Error details / upstream health

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut idx = 0;
    render_properties(frame, chunks[idx], st, state, theme);
    idx += 1;
    if has_sources {
        render_sources(frame, chunks[idx], st, state, theme);
        idx += 1;
    }
    render_stats(frame, chunks[idx], st, state, theme);
    idx += 1;
    if has_history {
        render_rich_refresh_history(frame, chunks[idx], st, state, theme);
    } else {
        render_recent_refreshes(frame, chunks[idx], st, state, theme);
    }
    idx += 1;
    if has_cdc_health {
        render_source_health(frame, chunks[idx], st, state, theme);
        idx += 1;
    }
    if has_errors {
        render_error_diagnosis(frame, chunks[idx], st, state, theme);
        idx += 1;
    }
    render_details(frame, chunks[idx], st, state, theme);
}

fn render_properties(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let status_style = theme.status_style(&st.status);
    let stale_style = if st.stale { theme.warning } else { theme.ok };

    let eff_str = if st.status == "ERROR" || st.status == "SUSPENDED" {
        "✗ error"
    } else if st.cascade_stale {
        "⚠ cascade-stale"
    } else {
        "✓ ok"
    };
    let eff_style = if st.cascade_stale {
        theme.warning
    } else {
        theme.ok
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Name:     ", theme.header),
            Span::raw(format!("{}.{}", st.schema, st.name)),
        ]),
        Line::from(vec![
            Span::styled(" Status:   ", theme.header),
            Span::styled(&st.status, status_style),
            Span::raw("    "),
            Span::styled("Effective: ", theme.header),
            Span::styled(eff_str, eff_style),
        ]),
        Line::from(vec![
            Span::styled(" Mode:     ", theme.header),
            Span::raw(&st.refresh_mode),
        ]),
    ];

    // Explain refresh mode (if cached)
    if let Some(explain) = state.explain_mode_cache.get(&st.name) {
        let downgraded = explain.configured_mode != explain.effective_mode;
        let mode_style = if downgraded { theme.warning } else { theme.ok };
        let mut mode_spans = vec![
            Span::styled(" Effective: ", theme.header),
            Span::styled(&explain.effective_mode, mode_style),
        ];
        if downgraded {
            mode_spans.push(Span::styled(" ↓ downgraded", theme.warning));
        }
        lines.push(Line::from(mode_spans));
        if let Some(ref reason) = explain.downgrade_reason {
            lines.push(Line::from(vec![
                Span::raw("   → "),
                Span::styled(reason.as_str(), theme.dim),
            ]));
        }
    }

    lines.extend([
        Line::from(vec![
            Span::styled(" Schedule: ", theme.header),
            Span::raw(st.schedule.as_deref().unwrap_or("-")),
        ]),
        Line::from(vec![
            Span::styled(" Stale:    ", theme.header),
            Span::styled(if st.stale { "yes" } else { "no" }, stale_style),
            Span::raw("  "),
            Span::styled("Staleness: ", theme.header),
            Span::raw(st.staleness.as_deref().unwrap_or("-")),
        ]),
        Line::from(vec![
            Span::styled(" Tier:     ", theme.header),
            Span::raw(st.tier.as_deref().unwrap_or("hot")),
            Span::raw("  "),
            Span::styled("Populated: ", theme.header),
            Span::raw(if st.is_populated { "yes" } else { "no" }),
        ]),
    ]);

    // Diamond group badge
    if let Some(dg) = state
        .diamond_groups
        .iter()
        .find(|d| d.member_name == st.name)
    {
        lines.push(Line::from(vec![
            Span::styled(" Diamond:  ", theme.header),
            Span::styled(
                format!("◆ Group {} (epoch {})", dg.group_id, dg.epoch),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    // SCC group badge
    if let Some(scc) = state
        .scc_groups
        .iter()
        .find(|s| s.members.contains(&st.name))
    {
        let converge_info = scc
            .last_converged_at
            .as_deref()
            .map(|t| format!(", last: {t}"))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(" SCC:      ", theme.header),
            Span::styled(
                format!(
                    "○ Group {} ({} members, {} iterations{})",
                    scc.scc_id, scc.member_count, scc.last_iterations, converge_info
                ),
                Style::default().fg(Color::Magenta),
            ),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" {} — Properties ", st.name),
            theme.title,
        ));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_stats(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    // Find efficiency data for this ST
    let eff = state
        .efficiency
        .iter()
        .find(|e| e.name == st.name && e.schema == st.schema);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Total refreshes:  ", theme.header),
            Span::raw(st.total_refreshes.to_string()),
        ]),
        Line::from(vec![
            Span::styled(" Failed refreshes: ", theme.header),
            Span::styled(
                st.failed_refreshes.to_string(),
                if st.failed_refreshes > 0 {
                    theme.error
                } else {
                    theme.ok
                },
            ),
        ]),
        Line::from(vec![
            Span::styled(" Avg duration:     ", theme.header),
            Span::raw(
                st.avg_duration_ms
                    .map(|ms| format!("{ms:.1} ms"))
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Last refresh:     ", theme.header),
            Span::raw(st.last_refresh_at.as_deref().unwrap_or("-")),
        ]),
    ];

    if let Some(e) = eff {
        lines.push(Line::from(vec![
            Span::styled(" Differential:     ", theme.header),
            Span::raw(format!("{}", e.diff_count)),
            Span::raw("  "),
            Span::styled("Full: ", theme.header),
            Span::raw(format!("{}", e.full_count)),
        ]));
        if let Some(ref speedup) = e.diff_speedup {
            lines.push(Line::from(vec![
                Span::styled(" Speedup (D→F):    ", theme.header),
                Span::styled(format!("{speedup}×"), theme.active),
            ]));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Refresh Statistics ", theme.title));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_recent_refreshes(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let entries: Vec<Line> = state
        .refresh_log
        .iter()
        .filter(|e| e.st_name == st.name)
        .take(5)
        .map(|e| {
            let status_style = match e.status.as_str() {
                "success" | "ok" => theme.ok,
                "error" | "failed" => theme.error,
                _ => theme.dim,
            };
            let dur = e
                .duration_ms
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_default();
            Line::from(vec![
                Span::styled(&e.timestamp, theme.dim),
                Span::raw("  "),
                Span::styled(&e.action, status_style),
                Span::raw("  "),
                Span::styled(&e.status, status_style),
                Span::raw("  "),
                Span::raw(dur),
            ])
        })
        .collect();

    let display = if entries.is_empty() {
        vec![Line::styled(" No recent refreshes", theme.dim)]
    } else {
        entries
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Recent Refreshes ", theme.title));

    let paragraph = Paragraph::new(display).block(block);
    frame.render_widget(paragraph, area);
}

fn render_sources(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let sources = match state.source_detail_cache.get(&st.name) {
        Some(s) => s,
        None => return,
    };

    let header = Row::new(
        ["Source", "Type", "CDC Mode", "Columns"]
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = sources
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(s.source_table.as_str()),
                Cell::from(s.source_type.as_str()),
                Cell::from(s.cdc_mode.as_str()),
                Cell::from(s.columns_used.as_deref().unwrap_or("-")),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(25),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Sources ({}) ", sources.len()),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_rich_refresh_history(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let entries = match state.refresh_history_cache.get(&st.name) {
        Some(e) => e,
        None => return,
    };

    let display: Vec<Line> = entries
        .iter()
        .take(7)
        .map(|e| {
            let status_style = match e.status.as_str() {
                "success" | "ok" => theme.ok,
                "error" | "failed" => theme.error,
                _ => theme.dim,
            };
            let dur = e
                .duration_ms
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_default();

            let mut spans = vec![
                Span::styled(&e.start_time, theme.dim),
                Span::raw("  "),
                Span::styled(&e.action, status_style),
                Span::raw("  "),
                Span::raw(dur),
            ];

            // Row counts
            if let Some(ins) = e.rows_inserted {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(format!("+{ins}"), theme.ok));
            }
            if let Some(del) = e.rows_deleted {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("-{del}"), theme.error));
            }
            if let Some(delta) = e.delta_row_count {
                spans.push(Span::raw(format!("  ({delta} Δ)")));
            }

            if e.was_full_fallback {
                spans.push(Span::styled(" (fallback)", theme.warning));
            }

            let icon = if e.status == "success" || e.status == "ok" {
                Span::styled(" ✓", theme.ok)
            } else {
                Span::styled(" ✗", theme.error)
            };
            spans.push(icon);

            Line::from(spans)
        })
        .collect();

    let display = if display.is_empty() {
        vec![Line::styled(" No refresh history", theme.dim)]
    } else {
        display
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Recent Refreshes ({}) ", entries.len()),
            theme.title,
        ));

    let paragraph = Paragraph::new(display).block(block);
    frame.render_widget(paragraph, area);
}

fn render_error_diagnosis(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let errors = match state.diagnosed_errors.get(&st.name) {
        Some(e) if !e.is_empty() => e,
        _ => return,
    };

    let mut lines = Vec::new();
    for err in errors.iter().take(5) {
        let type_style = match err.error_type.as_str() {
            "user" => Style::default().fg(Color::Cyan),
            "schema" => Style::default().fg(Color::Yellow),
            "correctness" => Style::default().fg(Color::Red),
            "performance" => Style::default().fg(Color::Magenta),
            "infrastructure" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            _ => theme.dim,
        };

        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(&err.event_time, theme.dim),
            Span::raw("  "),
            Span::styled(format!("[{}]", err.error_type), type_style),
            Span::raw("  "),
            Span::raw(&err.error_message),
        ]));
        lines.push(Line::from(vec![
            Span::raw("           → "),
            Span::styled(&err.remediation, theme.dim),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Error Diagnosis ", theme.title));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_details(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    let mut lines = Vec::new();

    if let Some(ref err) = st.last_error_message {
        lines.push(Line::from(Span::styled(" Last Error:", theme.error)));
        lines.push(Line::from(Span::raw(format!(" {err}"))));
        lines.push(Line::raw(""));
    }

    if st.consecutive_errors > 0 {
        lines.push(Line::from(vec![
            Span::styled(" Consecutive errors: ", theme.header),
            Span::styled(st.consecutive_errors.to_string(), theme.error),
        ]));
    }

    // Upstream health (F21)
    if st.cascade_stale {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            " ⚠ Upstream Health:",
            theme.warning,
        )));
        // Find error tables upstream via DAG
        for other in &state.stream_tables {
            if (other.status == "ERROR" || other.status == "SUSPENDED") && other.name != st.name {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled("✗ ", theme.error),
                    Span::styled(&other.name, theme.error.add_modifier(Modifier::BOLD)),
                    Span::raw(" — "),
                    Span::raw(other.last_error_message.as_deref().unwrap_or("(error)")),
                ]));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::styled(" No errors", theme.ok));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            " Error Details & Upstream Health ",
            theme.title,
        ));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_source_health(
    frame: &mut Frame,
    area: Rect,
    st: &StreamTableInfo,
    state: &AppState,
    theme: &Theme,
) {
    // Find CDC health entries for this stream table's sources
    let source_tables: Vec<&str> = state
        .cdc_buffers
        .iter()
        .filter(|b| b.stream_table == st.name)
        .map(|b| b.source_table.as_str())
        .collect();

    let entries: Vec<Line> = state
        .cdc_health
        .iter()
        .filter(|h| source_tables.contains(&h.source_table.as_str()))
        .map(|h| {
            let lag_str = h
                .lag_bytes
                .map(|b| {
                    if b < 1024 {
                        format!("{b} B")
                    } else if b < 1024 * 1024 {
                        format!("{:.1} KB", b as f64 / 1024.0)
                    } else {
                        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
                    }
                })
                .unwrap_or_else(|| "-".to_string());
            let lag_style = match h.lag_bytes {
                Some(b) if b > 10_000_000 => theme.error,
                Some(b) if b > 1_000_000 => theme.warning,
                _ => theme.ok,
            };
            let alert_span = h
                .alert
                .as_deref()
                .map(|a| Span::styled(format!(" ⚠ {a}"), theme.warning))
                .unwrap_or_else(|| Span::raw(""));
            Line::from(vec![
                Span::styled(format!(" {} ", h.source_table), theme.header),
                Span::raw(format!("[{}] ", h.cdc_mode)),
                Span::raw("lag: "),
                Span::styled(lag_str, lag_style),
                alert_span,
            ])
        })
        .collect();

    let display = if entries.is_empty() {
        vec![Line::styled(" No CDC health data for sources", theme.dim)]
    } else {
        entries
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(" Source CDC Health ", theme.title));

    frame.render_widget(Paragraph::new(display).block(block), area);
}
