pub mod alert;
pub mod cdc;
pub mod config;
pub mod dashboard;
pub mod delta_inspector;
pub mod detail;
pub mod diagnostics;
pub mod fuse;
pub mod graph;
pub mod health;
pub mod help;
pub mod issues;
pub mod refresh_log;
pub mod watermarks;
pub mod workers;

#[cfg(test)]
mod snapshot_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use crate::app::View;
    use crate::state::AppState;
    use crate::test_fixtures;
    use crate::theme::Theme;

    /// Render into a TestBackend and return the buffer content as a string.
    fn render_to_string(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut ratatui::Frame, Rect, &AppState, &Theme),
    ) -> String {
        let state = test_fixtures::sample_state();
        let theme = Theme::default_dark();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area, &state, &theme);
            })
            .unwrap();
        buffer_to_string(terminal.backend())
    }

    fn render_empty_to_string(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut ratatui::Frame, Rect, &AppState, &Theme),
    ) -> String {
        let state = test_fixtures::empty_state();
        let theme = Theme::default_dark();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area, &state, &theme);
            })
            .unwrap();
        buffer_to_string(terminal.backend())
    }

    fn buffer_to_string(backend: &TestBackend) -> String {
        let buf = backend.buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = buf.cell((x, y)).unwrap();
                s.push_str(cell.symbol());
            }
            // Trim trailing whitespace per line for stable snapshots
            let trimmed = s.rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line = &s[trimmed..];
            let trimmed_line = line.trim_end();
            s.truncate(trimmed + trimmed_line.len());
            s.push('\n');
        }
        s
    }

    // ── Dashboard view ───────────────────────────────────────────────
    // Dashboard uses Constraint::Min(20) which can produce non-deterministic
    // column widths due to ratatui's layout cache. We use content assertions
    // instead of exact snapshots.

    #[test]
    fn test_dashboard_standard_80x24() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::dashboard::render(frame, area, state, theme, 0, None);
        });
        assert!(
            output.contains("Stream Tables"),
            "should contain ribbon title"
        );
        assert!(output.contains("5 total"), "should show total count");
        assert!(output.contains("3 active"), "should show active count");
        assert!(output.contains("2 error"), "should show error count");
        assert!(
            output.contains("revenue_daily"),
            "should list error table first"
        );
        assert!(output.contains("orders_live"), "should list active table");
        assert!(output.contains("DIFF"), "should show refresh mode");
    }

    #[test]
    fn test_dashboard_wide_150x40() {
        let output = render_to_string(150, 40, |frame, area, state, theme| {
            super::dashboard::render(frame, area, state, theme, 0, None);
        });
        assert!(output.contains("Stream Tables"), "should contain ribbon");
        assert!(
            output.contains("DAG Mini-Map"),
            "wide layout should show DAG minimap"
        );
        assert!(
            output.contains("Issues"),
            "wide layout should show issues sidebar"
        );
        assert!(
            output.contains("revenue_daily"),
            "should list stream tables"
        );
    }

    #[test]
    fn test_dashboard_with_filter() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::dashboard::render(frame, area, state, theme, 0, Some("orders"));
        });
        assert!(
            output.contains("filter: orders"),
            "should show active filter"
        );
        assert!(
            output.contains("1 of 5"),
            "should show filtered count vs total"
        );
        assert!(output.contains("orders_live"), "should show matching table");
    }

    #[test]
    fn test_dashboard_empty_state() {
        let output = render_empty_to_string(80, 24, |frame, area, state, theme| {
            super::dashboard::render(frame, area, state, theme, 0, None);
        });
        assert!(output.contains("0 total"), "should show zero total");
        assert!(output.contains("0 active"), "should show zero active");
        assert!(output.contains("0 of 0"), "should show empty table count");
    }

    // ── Detail view ──────────────────────────────────────────────────

    #[test]
    fn test_detail_with_selected_table() {
        let output = render_to_string(80, 40, |frame, area, state, theme| {
            super::detail::render(frame, area, state, theme, Some(0));
        });
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_detail_no_selection() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::detail::render(frame, area, state, theme, None);
        });
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_detail_error_table() {
        let output = render_to_string(80, 40, |frame, area, state, theme| {
            super::detail::render(frame, area, state, theme, Some(1));
        });
        insta::assert_snapshot!(output);
    }

    // ── Graph view ───────────────────────────────────────────────────

    #[test]
    fn test_graph_view() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::graph::render(frame, area, state, theme, 0);
        });
        insta::assert_snapshot!(output);
    }

    // ── Refresh Log view ─────────────────────────────────────────────

    #[test]
    fn test_refresh_log_view() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::refresh_log::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Diagnostics view ─────────────────────────────────────────────

    #[test]
    fn test_diagnostics_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::diagnostics::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── CDC view ─────────────────────────────────────────────────────

    #[test]
    fn test_cdc_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::cdc::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Config view ──────────────────────────────────────────────────

    #[test]
    fn test_config_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::config::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Health view ──────────────────────────────────────────────────

    #[test]
    fn test_health_view() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::health::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Alert view ───────────────────────────────────────────────────

    #[test]
    fn test_alert_view_empty() {
        let output = render_empty_to_string(80, 24, |frame, area, state, theme| {
            super::alert::render(frame, area, state, theme);
        });
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_alert_view_with_data() {
        let theme = Theme::default_dark();
        let mut state = test_fixtures::sample_state();
        state.alerts.push(crate::state::AlertEvent {
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            severity: "warning".to_string(),
            message: "Refresh failed for orders_live".to_string(),
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::alert::render(frame, area, &state, &theme);
            })
            .unwrap();
        let output = buffer_to_string(terminal.backend());
        insta::assert_snapshot!(output);
    }

    // ── Workers view ─────────────────────────────────────────────────

    #[test]
    fn test_workers_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::workers::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Fuse view ────────────────────────────────────────────────────

    #[test]
    fn test_fuse_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::fuse::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Watermarks view ──────────────────────────────────────────────

    #[test]
    fn test_watermarks_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::watermarks::render(frame, area, state, theme, 0, 0);
        });
        insta::assert_snapshot!(output);
    }

    // ── Delta Inspector view ─────────────────────────────────────────

    #[test]
    fn test_delta_inspector_with_selection() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::delta_inspector::render(frame, area, state, theme, Some(0), 0);
        });
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_delta_inspector_no_selection() {
        let output = render_to_string(80, 24, |frame, area, state, theme| {
            super::delta_inspector::render(frame, area, state, theme, None, 0);
        });
        insta::assert_snapshot!(output);
    }

    // ── Issues view ──────────────────────────────────────────────────

    #[test]
    fn test_issues_view() {
        let output = render_to_string(100, 24, |frame, area, state, theme| {
            super::issues::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_issues_view_empty() {
        let output = render_empty_to_string(80, 24, |frame, area, state, theme| {
            super::issues::render(frame, area, state, theme, 0, None);
        });
        insta::assert_snapshot!(output);
    }

    // ── Help overlay ─────────────────────────────────────────────────

    #[test]
    fn test_help_overlay_dashboard() {
        let theme = Theme::default_dark();
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::help::render(frame, area, &theme, View::Dashboard);
            })
            .unwrap();
        let output = buffer_to_string(terminal.backend());
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_help_overlay_health() {
        let theme = Theme::default_dark();
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::help::render(frame, area, &theme, View::Health);
            })
            .unwrap();
        let output = buffer_to_string(terminal.backend());
        insta::assert_snapshot!(output);
    }
}
