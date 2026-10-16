use ratatui::style::{Color, Modifier, Style};

/// Neutral dark surfaces with a magenta accent, shared with inbox and ot.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub bg_raised: Color,
    pub bg_selected: Color,
    pub bg_field: Color,
    pub border: Color,
    pub text: Color,
    pub dim: Color,
    pub faint: Color,
    pub accent: Color,
    pub ok: Color,
    #[allow(dead_code)]
    pub warn: Color,
    pub danger: Color,
}

impl Theme {
    pub fn grok_night() -> Self {
        Self {
            bg: Color::Rgb(22, 22, 24),
            bg_raised: Color::Rgb(30, 30, 33),
            bg_selected: Color::Rgb(48, 32, 42),
            bg_field: Color::Rgb(34, 34, 38),
            border: Color::Rgb(58, 56, 60),
            text: Color::Rgb(232, 230, 226),
            dim: Color::Rgb(154, 150, 144),
            faint: Color::Rgb(108, 104, 99),
            accent: Color::Rgb(226, 120, 166),
            ok: Color::Rgb(126, 184, 146),
            warn: Color::Rgb(214, 176, 108),
            danger: Color::Rgb(216, 112, 118),
        }
    }

    pub fn fg(self, color: Color) -> Style {
        Style::new().fg(color)
    }

    pub fn text(self) -> Style {
        self.fg(self.text)
    }

    pub fn dim(self) -> Style {
        self.fg(self.dim)
    }

    pub fn faint(self) -> Style {
        self.fg(self.faint)
    }

    pub fn ok(self) -> Style {
        self.fg(self.ok)
    }

    pub fn accent(self) -> Style {
        self.fg(self.accent)
    }

    pub fn bold_accent(self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    pub fn bold(self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }
}
