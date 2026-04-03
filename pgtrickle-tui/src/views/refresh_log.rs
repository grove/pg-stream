use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

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
    let f = filter.unwrap_or("").to_lowercase();
    let lines: Vec<Line> = state
        .refresh_log
        .iter()
        .filter(|entry| f.is_empty() || entry.st_name.to_lowercase().contains(&f))
        .enumerate()
        .map(|(i, entry)| {
            let style = if i == selected {
                theme.selected
            } else {
                match entry.status.as_str() {
                    "success" | "ok" => theme.ok,
                    "error" | "failed" => theme.error,
                    _ => theme.dim,
                }
            };

            let dur = entry
                .duration_ms
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_default();

            Line::from(vec![
                Span::styled(&entry.timestamp, theme.dim),
                Span::raw("  "),
                Span::styled(&entry.st_name, style.add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(&entry.action, style),
                Span::raw("  "),
                Span::styled(&entry.status, style),
                Span::raw("  "),
                Span::raw(dur),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Refresh Log ({} entries) ", state.refresh_log.len()),
            theme.title,
        ));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}
