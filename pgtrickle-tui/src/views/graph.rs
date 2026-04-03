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

        vec![Line::from(spans)]
    } else {
        vec![Line::styled(" No node selected", theme.dim)]
    };

    let meta_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border);
    frame.render_widget(Paragraph::new(meta_lines).block(meta_block), chunks[1]);
}
