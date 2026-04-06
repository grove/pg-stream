use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::state::AppState;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme, selected: usize) {
    // Build diamond/SCC lookup for node annotation
    let diamond_lookup: std::collections::HashMap<&str, i32> = state
        .diamond_groups
        .iter()
        .map(|d| (d.member_name.as_str(), d.group_id))
        .collect();

    let scc_lookup: std::collections::HashMap<&str, i32> = state
        .scc_groups
        .iter()
        .flat_map(|s| s.members.split(", ").map(move |m| (m, s.scc_id)))
        .collect();

    // Freshness lookup: schema.name -> StreamTableInfo
    // dag_edges use schema-qualified node names (e.g. "public.category_summary").
    let st_lookup: std::collections::HashMap<String, &crate::state::StreamTableInfo> = state
        .stream_tables
        .iter()
        .map(|st| (format!("{}.{}", st.schema, st.name), st))
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let lines: Vec<Line> = state
        .dag_edges
        .iter()
        .enumerate()
        .map(|(i, edge)| {
            let style = if i == selected {
                theme.selected
            } else {
                edge.status
                    .as_deref()
                    .map(|s| theme.status_style(s))
                    .unwrap_or_default()
            };

            let mut spans = vec![Span::styled(edge.tree_line.as_str(), style)];

            // Diamond marker
            if let Some(&gid) = diamond_lookup.get(edge.node.as_str()) {
                spans.push(Span::styled(
                    format!(" ◆{gid}"),
                    Style::default().fg(Color::Cyan),
                ));
            }

            // SCC marker
            if let Some(&sid) = scc_lookup.get(edge.node.as_str()) {
                spans.push(Span::styled(
                    format!(" ○{sid}"),
                    Style::default().fg(Color::Magenta),
                ));
            }

            // Freshness badge
            if let Some(st) = st_lookup.get(edge.node.as_str()) {
                let (icon, badge_style) = if st.status == "ERROR" || st.status == "SUSPENDED" {
                    ("✗", theme.error)
                } else if st.cascade_stale || st.stale {
                    ("⚠", theme.warning)
                } else {
                    ("✓", theme.ok)
                };
                let age = st
                    .staleness
                    .as_deref()
                    .map(|s| format!("  {} {}", format_age(s), icon))
                    .unwrap_or_else(|| format!("  {icon}"));
                spans.push(Span::styled(age, badge_style));
            }

            Line::from(spans)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Dependency Graph ({} nodes) ", state.dag_edges.len()),
            theme.title,
        ));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, chunks[0]);

    // Selected node metadata
    let meta_lines = if let Some(edge) = state.dag_edges.get(selected) {
        let mut spans = vec![
            Span::styled(" Selected: ", theme.header),
            Span::raw(&edge.node),
        ];

        if let Some(&gid) = diamond_lookup.get(edge.node.as_str())
            && let Some(dg) = state.diamond_groups.iter().find(|d| d.group_id == gid)
        {
            spans.push(Span::styled(
                format!("  ◆ Diamond group {} (epoch: {})", dg.group_id, dg.epoch),
                Style::default().fg(Color::Cyan),
            ));
        }

        if let Some(&sid) = scc_lookup.get(edge.node.as_str())
            && let Some(scc) = state.scc_groups.iter().find(|s| s.scc_id == sid)
        {
            let converge = scc
                .last_converged_at
                .as_deref()
                .map(|t| format!(", converged {t}"))
                .unwrap_or_default();
            spans.push(Span::styled(
                format!(
                    "  ○ SCC group {} ({} members, {} iterations{})",
                    scc.scc_id, scc.member_count, scc.last_iterations, converge
                ),
                Style::default().fg(Color::Magenta),
            ));
        }

        // Freshness detail for the selected node
        if let Some(st) = st_lookup.get(edge.node.as_str()) {
            let age_str = st
                .staleness
                .as_deref()
                .map(|s| format!("age {}", format_age(s)))
                .unwrap_or_else(|| "age unknown".to_string());
            let last = st
                .last_refresh_at
                .as_deref()
                .map(|t| format!(", last refresh {t}"))
                .unwrap_or_default();
            let (freshness_label, freshness_style) =
                if st.status == "ERROR" || st.status == "SUSPENDED" {
                    (format!("  ✗ {}{}", age_str, last), theme.error)
                } else if st.cascade_stale {
                    (
                        format!("  ⚠ cascade-stale  {}{}", age_str, last),
                        theme.warning,
                    )
                } else if st.stale {
                    (format!("  ⚠ stale  {}{}", age_str, last), theme.warning)
                } else {
                    (format!("  ✓ fresh  {}{}", age_str, last), theme.ok)
                };
            spans.push(Span::styled(freshness_label, freshness_style));
        }

        vec![Line::from(spans)]
    } else {
        vec![Line::styled(" No node selected", theme.dim)]
    };

    let meta_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border);
    frame.render_widget(Paragraph::new(meta_lines).block(meta_block), chunks[1]);
}

/// Convert a raw seconds string (e.g. "49253s") to a compact human-readable age.
fn format_age(staleness: &str) -> String {
    let secs: f64 = staleness.trim_end_matches('s').parse().unwrap_or(0.0);
    if secs >= 3600.0 {
        format!("{:.0}h", secs / 3600.0)
    } else if secs >= 60.0 {
        format!("{:.0}m", secs / 60.0)
    } else {
        format!("{:.0}s", secs)
    }
}
