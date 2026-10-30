//! Colors and symbols, adapted to what the terminal can show.
//!
//! The interface is black, white and gray. Blue is reserved for the Nix identity.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

/// What a terminal can show, from its environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSupport {
    TrueColor,
    Indexed256,
    Basic,
    None,
}

const TRUE_COLOR_TERMINALS: [&str; 7] = [
    "alacritty",
    "foot",
    "ghostty",
    "iterm",
    "kitty",
    "wezterm",
    "truecolor",
];

impl ColorSupport {
    /// Reads `NO_COLOR`, `COLORTERM` and `TERM` through `variable`.
    pub fn detect(variable: impl Fn(&str) -> Option<String>) -> Self {
        if variable("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return Self::None;
        }
        let term = variable("TERM").unwrap_or_default().to_lowercase();
        if term.is_empty() || term == "dumb" {
            return Self::None;
        }
        let colorterm = variable("COLORTERM").unwrap_or_default().to_lowercase();
        if colorterm == "truecolor"
            || colorterm == "24bit"
            || TRUE_COLOR_TERMINALS.iter().any(|name| term.contains(name))
        {
            Self::TrueColor
        } else if term.contains("256color") {
            Self::Indexed256
        } else {
            Self::Basic
        }
    }
}

/// Whether the terminal's character set has box drawing and block characters. Only an
/// explicit other character set counts against it, as most systems today are UTF-8.
pub fn supports_unicode(variable: impl Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| variable(name).filter(|value| !value.is_empty()))
        .is_none_or(|locale| {
            let locale = locale.to_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        })
}

/// Everything the views need to draw consistently.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub support: ColorSupport,
    pub unicode: bool,
    background: Color,
    text: Color,
    strong: Color,
    muted: Color,
    line: Color,
    accent: Color,
    accent_dark: Color,
}

impl Theme {
    /// The theme for the given terminal.
    pub fn new(support: ColorSupport, unicode: bool) -> Self {
        let (background, text, strong, muted, line, accent, accent_dark) = match support {
            ColorSupport::TrueColor => (
                Color::Rgb(0, 0, 0),
                Color::Rgb(225, 225, 225),
                Color::Rgb(255, 255, 255),
                Color::Rgb(140, 140, 140),
                Color::Rgb(72, 72, 72),
                Color::Rgb(0x7e, 0xba, 0xe4),
                Color::Rgb(0x52, 0x77, 0xc3),
            ),
            ColorSupport::Indexed256 => (
                Color::Indexed(16),
                Color::Indexed(254),
                Color::Indexed(231),
                Color::Indexed(245),
                Color::Indexed(238),
                Color::Indexed(110),
                Color::Indexed(67),
            ),
            ColorSupport::Basic => (
                Color::Black,
                Color::Gray,
                Color::White,
                Color::DarkGray,
                Color::DarkGray,
                Color::Blue,
                Color::Blue,
            ),
            ColorSupport::None => (
                Color::Reset,
                Color::Reset,
                Color::Reset,
                Color::Reset,
                Color::Reset,
                Color::Reset,
                Color::Reset,
            ),
        };
        Self {
            support,
            unicode,
            background,
            text,
            strong,
            muted,
            line,
            accent,
            accent_dark,
        }
    }

    /// The theme for this process's environment.
    pub fn from_environment() -> Self {
        let variable = |name: &str| std::env::var(name).ok();
        Self::new(ColorSupport::detect(variable), supports_unicode(variable))
    }

    /// Normal text.
    pub fn text(&self) -> Style {
        Style::new().fg(self.text).bg(self.background)
    }

    /// Titles and what needs attention.
    pub fn strong(&self) -> Style {
        Style::new()
            .fg(self.strong)
            .bg(self.background)
            .add_modifier(Modifier::BOLD)
    }

    /// Secondary information. Without colors, it stands out by being dim.
    pub fn muted(&self) -> Style {
        let style = Style::new().fg(self.muted).bg(self.background);
        if self.support == ColorSupport::None {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Borders and dividers.
    pub fn line(&self) -> Style {
        Style::new().fg(self.line).bg(self.background)
    }

    /// The Nix identity, and nothing else.
    pub fn accent(&self) -> Style {
        Style::new()
            .fg(self.accent)
            .bg(self.background)
            .add_modifier(Modifier::BOLD)
    }

    /// The selected row: inverted, which works with any set of colors.
    pub fn selected(&self) -> Style {
        self.text().add_modifier(Modifier::REVERSED)
    }

    /// The two blues of the logo, for terminals that cannot show its real colors.
    pub fn logo_tones(&self) -> (Color, Color) {
        (self.accent, self.accent_dark)
    }

    pub fn background(&self) -> Color {
        self.background
    }

    /// Square borders.
    pub fn border(&self) -> border::Set<'static> {
        if self.unicode {
            border::PLAIN
        } else {
            border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                vertical_left: "|",
                vertical_right: "|",
                horizontal_top: "-",
                horizontal_bottom: "-",
            }
        }
    }

    pub fn horizontal(&self) -> &'static str {
        if self.unicode { "─" } else { "-" }
    }

    pub fn joint_right(&self) -> &'static str {
        if self.unicode { "├" } else { "+" }
    }

    pub fn joint_left(&self) -> &'static str {
        if self.unicode { "┤" } else { "+" }
    }

    pub fn joint_down(&self) -> &'static str {
        if self.unicode { "┬" } else { "+" }
    }

    pub fn joint_up(&self) -> &'static str {
        if self.unicode { "┴" } else { "+" }
    }

    pub fn vertical(&self) -> &'static str {
        if self.unicode { "│" } else { "|" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(values: &'static [(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn color_support_comes_from_the_environment() {
        let cases: [(&'static [(&'static str, &'static str)], ColorSupport); 7] = [
            (
                &[("TERM", "xterm"), ("COLORTERM", "truecolor")],
                ColorSupport::TrueColor,
            ),
            (&[("TERM", "xterm-kitty")], ColorSupport::TrueColor),
            (&[("TERM", "xterm-256color")], ColorSupport::Indexed256),
            (&[("TERM", "linux")], ColorSupport::Basic),
            (&[("TERM", "dumb")], ColorSupport::None),
            (&[], ColorSupport::None),
            (
                &[("TERM", "xterm-kitty"), ("NO_COLOR", "1")],
                ColorSupport::None,
            ),
        ];
        for (values, expected) in cases {
            assert_eq!(ColorSupport::detect(env(values)), expected, "{values:?}");
        }
    }

    #[test]
    fn unicode_is_assumed_unless_another_character_set_is_named() {
        assert!(supports_unicode(env(&[])));
        assert!(supports_unicode(env(&[("LANG", "en_US.UTF-8")])));
        assert!(supports_unicode(env(&[("LANG", "pt_BR.utf8")])));
        assert!(!supports_unicode(env(&[("LANG", "C")])));
        assert!(!supports_unicode(env(&[
            ("LC_ALL", "POSIX"),
            ("LANG", "en_US.UTF-8")
        ])));
    }

    #[test]
    fn plain_terminals_use_plain_borders() {
        let theme = Theme::new(ColorSupport::Basic, false);
        assert_eq!(theme.border().top_left, "+");
        assert_eq!(theme.horizontal(), "-");
    }
}
