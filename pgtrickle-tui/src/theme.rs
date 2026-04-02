use ratatui::style::{Color, Modifier, Style};

#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub name: &'static str,
    pub active: Style,
    pub error: Style,
    pub suspended: Style,
    pub paused: Style,
    pub selected: Style,
    pub header: Style,
    pub border: Style,
    pub warning: Style,
    pub ok: Style,
    pub title: Style,
    pub footer: Style,
    pub footer_active: Style,
    pub dim: Style,
}

impl Theme {
    pub fn default_dark() -> Self {
        Self {
            name: "default",
            active: Style::default().fg(Color::Green),
            error: Style::default().fg(Color::Red),
            suspended: Style::default().fg(Color::Yellow),
            paused: Style::default().fg(Color::DarkGray),
            selected: Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            header: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            border: Style::default().fg(Color::DarkGray),
            warning: Style::default().fg(Color::Yellow),
            ok: Style::default().fg(Color::Green),
            title: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            footer: Style::default().fg(Color::DarkGray),
            footer_active: Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            dim: Style::default().fg(Color::DarkGray),
        }
    }

    pub fn status_style(&self, status: &str) -> Style {
        match status {
            "ACTIVE" => self.active,
            "ERROR" => self.error,
            "SUSPENDED" => self.suspended,
            "PAUSED" | "FROZEN" => self.paused,
            _ => Style::default(),
        }
    }

    pub fn severity_style(&self, severity: &str) -> Style {
        match severity {
            "critical" => self.error,
            "warning" => self.warning,
            "ok" => self.ok,
            _ => Style::default(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_theme_is_default_dark() {
        let theme = Theme::default();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn test_status_style_active() {
        let theme = Theme::default_dark();
        let style = theme.status_style("ACTIVE");
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn test_status_style_error() {
        let theme = Theme::default_dark();
        let style = theme.status_style("ERROR");
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn test_status_style_suspended() {
        let theme = Theme::default_dark();
        let style = theme.status_style("SUSPENDED");
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_status_style_paused() {
        let theme = Theme::default_dark();
        let style = theme.status_style("PAUSED");
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_status_style_frozen() {
        let theme = Theme::default_dark();
        let style = theme.status_style("FROZEN");
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_status_style_unknown() {
        let theme = Theme::default_dark();
        let style = theme.status_style("UNKNOWN");
        assert_eq!(style, Style::default());
    }

    #[test]
    fn test_severity_style_critical() {
        let theme = Theme::default_dark();
        let style = theme.severity_style("critical");
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn test_severity_style_warning() {
        let theme = Theme::default_dark();
        let style = theme.severity_style("warning");
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_severity_style_ok() {
        let theme = Theme::default_dark();
        let style = theme.severity_style("ok");
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn test_severity_style_unknown() {
        let theme = Theme::default_dark();
        let style = theme.severity_style("info");
        assert_eq!(style, Style::default());
    }
}
