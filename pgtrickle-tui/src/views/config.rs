use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

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
    let header = Row::new(
        ["Parameter", "Value", "Unit", "Description"]
            .iter()
            .map(|h| Cell::from(*h).style(theme.header)),
    )
    .height(1);

    let rows: Vec<Row> = state
        .guc_params
        .iter()
        .filter(|g| {
            f.is_empty()
                || g.name.to_lowercase().contains(&f)
                || g.short_desc.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, g)| {
            let style = if i == selected {
                theme.selected
            } else {
                ratatui::style::Style::default()
            };
            Row::new(vec![
                Cell::from(g.name.as_str()),
                Cell::from(g.setting.as_str()),
                Cell::from(g.unit.as_deref().unwrap_or("")),
                Cell::from(g.short_desc.as_str()),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(35),
        Constraint::Length(15),
        Constraint::Length(8),
        Constraint::Min(30),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(
            format!(" Configuration ({} params) ", state.guc_params.len()),
            theme.title,
        ));

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}
