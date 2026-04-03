use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    render_workers(frame, chunks[0], state, theme, selected, filter);
    render_job_queue(frame, chunks[1], state, theme);
}

fn render_workers(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    selected: usize,
    filter: Option<&str>,
) {
    let f = filter.unwrap_or("").to_lowercase();
    let header = Row::new(vec!["Worker", "State", "Table", "Started", "Duration"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .workers
        .iter()
        .filter(|w| {
            f.is_empty()
                || w.table_name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&f))
                || w.state.to_lowercase().contains(&f)
        })
        .enumerate()
        .map(|(i, w)| {
            let state_style = match w.state.as_str() {
                "idle" => theme.dim,
                "busy" | "running" => theme.active,
                _ => theme.warning,
            };
            let row = Row::new(vec![
                Cell::from(format!("#{}", w.worker_id)),
                Cell::from(w.state.as_str()).style(state_style),
                Cell::from(w.table_name.as_deref().unwrap_or("-")),
                Cell::from(w.started_at.as_deref().unwrap_or("-")),
                Cell::from(
                    w.duration_ms
                        .map(|d| format!("{d:.0}ms"))
                        .unwrap_or_default(),
                ),
            ]);
            if i == selected {
                row.style(theme.selected)
            } else {
                row
            }
        })
        .collect();

    let active = state.workers.iter().filter(|w| w.state != "idle").count();

    let widths = [
        Constraint::Percentage(12),
        Constraint::Percentage(15),
        Constraint::Percentage(30),
        Constraint::Percentage(23),
        Constraint::Percentage(20),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                format!(" Workers ({active}/{} active) ", state.workers.len()),
                theme.title,
            )),
    );

    frame.render_widget(table, area);
}

fn render_job_queue(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let header = Row::new(vec!["#", "Table", "Priority", "Queued At", "Wait"])
        .style(theme.header)
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .job_queue
        .iter()
        .map(|j| {
            Row::new(vec![
                Cell::from(format!("{}", j.position)),
                Cell::from(j.table_name.as_str()),
                Cell::from(format!("{}", j.priority)),
                Cell::from(j.queued_at.as_str()),
                Cell::from(j.wait_ms.map(|w| format!("{w:.0}ms")).unwrap_or_default()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(8),
        Constraint::Percentage(32),
        Constraint::Percentage(15),
        Constraint::Percentage(25),
        Constraint::Percentage(20),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(
                format!(" Job Queue ({}) ", state.job_queue.len()),
                theme.title,
            )),
    );

    frame.render_widget(table, area);
}
