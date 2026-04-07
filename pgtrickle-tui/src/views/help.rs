use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::View;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, current_view: View) {
    let title = format!(
        " Help — {} — press ? or Esc to close ",
        current_view.label()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(title, theme.title));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    let navigation = Paragraph::new(vec![
        Line::styled(" Navigation", theme.header),
        Line::raw(""),
        Line::raw("  1        Dashboard"),
        Line::raw("  2        Detail view"),
        Line::raw("  3        Dependency graph"),
        Line::raw("  4        Refresh log"),
        Line::raw("  5        Diagnostics"),
        Line::raw("  6        CDC health"),
        Line::raw("  7        Configuration"),
        Line::raw("  8        Health checks"),
        Line::raw("  9        Alerts"),
        Line::raw("  w        Workers & job queue"),
        Line::raw("  f        Fuse & circuit breaker"),
        Line::raw("  m        Watermarks & gating"),
        Line::raw("  d        Delta SQL inspector"),
        Line::raw("  g        Graph view"),
        Line::raw("  i        Issues"),
        Line::raw(""),
        Line::raw("  j / ↓    Move down"),
        Line::raw("  k / ↑    Move up"),
        Line::raw("  Enter    Drill into detail"),
        Line::raw("  Esc      Back / close overlay"),
        Line::raw("  Tab      Next pane"),
    ]);

    let context_lines = context_help(current_view);
    let mut action_lines = vec![
        Line::styled(" Actions", theme.header),
        Line::raw(""),
        Line::raw("  r        Refresh selected"),
        Line::raw("  R        Refresh all"),
        Line::raw("  p        Pause selected"),
        Line::raw("  P        Resume selected"),
        Line::raw("  s / S    Sort / reverse sort"),
        Line::raw("  t        Toggle light/dark theme"),
        Line::raw("  Ctrl+R   Force poll now"),
        Line::raw("  Ctrl+E   Export view to JSON"),
        Line::raw("  /        Filter tables"),
        Line::raw("  :        Command palette"),
        Line::raw("  ?        Toggle help"),
        Line::raw("  q        Quit"),
        Line::raw("  Ctrl+C   Quit"),
    ];

    if !context_lines.is_empty() {
        action_lines.push(Line::raw(""));
        action_lines.push(Line::styled(
            format!(" {} Tips", current_view.label()),
            theme.header,
        ));
        action_lines.push(Line::raw(""));
        for tip in context_lines {
            action_lines.push(Line::raw(format!("  {tip}")));
        }
    }

    frame.render_widget(navigation, cols[0]);
    frame.render_widget(Paragraph::new(action_lines), cols[1]);
}

fn context_help(view: View) -> Vec<&'static str> {
    match view {
        View::Dashboard => vec![
            "/ to filter, Enter for detail",
            "s to cycle sort, S to reverse",
            "r to refresh selected, R for all",
            "EFF column shows cascade staleness",
        ],
        View::Detail => vec![
            "e to show DDL export overlay",
            "r to refresh, p to pause, P resume",
            "Shows source CDC health if available",
        ],
        View::Graph => vec!["ASCII dependency graph", "Arrows show refresh dependencies"],
        View::RefreshLog => vec!["Recent refresh history", "Duration and row counts shown"],
        View::Cdc => vec![
            "Tab to move between sections",
            "↑↓ to navigate rows in section",
            "Esc to return focus to Buffers",
        ],
        View::Health => vec![
            "Overall system health summary",
            "Critical issues need attention",
        ],
        View::Fuse => vec![
            "A to re-arm selected fuse",
            "Circuit breakers protect tables",
        ],
        View::Watermarks => vec![
            "Tab to switch groups/gates tabs",
            "g to gate/ungate source (tab 2)",
        ],
        View::DeltaInspector => vec![
            "Enter to reload delta SQL",
            "e to show CREATE statement (DDL)",
            "Tab to switch Delta SQL / Auxiliary Columns",
        ],
        View::Diagnostics => vec![
            "Signal breakdown shown for selected",
            "Bar chart shows scoring weights",
        ],
        View::Issues => vec![
            "All detected DAG issues",
            "Sorted by severity and blast radius",
        ],
        _ => vec![],
    }
}
