//! Draws the Nix logo, as well as the terminal allows.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::logo_data::{LOGO_LARGE, LOGO_MEDIUM};
use super::theme::{ColorSupport, Theme};

const UPPER_HALF: &str = "▀";
const LOWER_HALF: &str = "▄";
const FULL: &str = "█";
/// Below this brightness, a pixel is mostly background.
const VISIBLE: u32 = 90;
/// Above this one, it belongs to the light blue part of the logo.
const LIGHT: u32 = 170;
/// Text for terminals that cannot draw the logo.
const TEXT_LOGO: &str = "NIX";

/// Terminal rows from which the larger logo fits comfortably.
const LARGE_FROM_ROWS: u16 = 34;
/// Terminal rows from which any logo fits and still leaves room for the list.
const LOGO_FROM_ROWS: u16 = 20;

/// Which logo to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSize {
    Large,
    Medium,
    Text,
}

impl LogoSize {
    /// The best logo for a terminal of `rows` rows.
    pub fn for_terminal(theme: &Theme, rows: u16) -> Self {
        if !theme.unicode || theme.support == ColorSupport::None || rows < LOGO_FROM_ROWS {
            Self::Text
        } else if rows >= LARGE_FROM_ROWS {
            Self::Large
        } else {
            Self::Medium
        }
    }

    /// Cells taken: width and height.
    pub fn cells(self) -> (u16, u16) {
        match self {
            Self::Large => (14, 6),
            Self::Medium => (11, 5),
            Self::Text => (3, 1),
        }
    }
}

/// Draws the logo with its top left corner at the start of `area`.
pub fn draw(size: LogoSize, theme: &Theme, area: Rect, buffer: &mut Buffer) {
    match size {
        LogoSize::Text => {
            buffer.set_string(area.x, area.y, TEXT_LOGO, theme.accent());
        }
        LogoSize::Large => draw_pixels(
            LOGO_LARGE.iter().map(<[u32; 14]>::as_slice),
            theme,
            area,
            buffer,
        ),
        LogoSize::Medium => draw_pixels(
            LOGO_MEDIUM.iter().map(<[u32; 11]>::as_slice),
            theme,
            area,
            buffer,
        ),
    }
}

fn draw_pixels<'a>(
    rows: impl Iterator<Item = &'a [u32]>,
    theme: &Theme,
    area: Rect,
    buffer: &mut Buffer,
) {
    let rows: Vec<&[u32]> = rows.collect();
    for (row, pair) in rows.chunks(2).enumerate() {
        let [upper, lower] = pair else { continue };
        for (column, (top, bottom)) in upper.iter().zip(*lower).enumerate() {
            let (Ok(row), Ok(column)) = (u16::try_from(row), u16::try_from(column)) else {
                continue;
            };
            let (x, y) = (area.x + column, area.y + row);
            if x >= area.right() || y >= area.bottom() {
                continue;
            }
            let (symbol, style) = cell(*top, *bottom, theme);
            buffer.set_string(x, y, symbol, style);
        }
    }
}

/// The character and colors that show two pixels in one cell.
fn cell(top: u32, bottom: u32, theme: &Theme) -> (&'static str, Style) {
    let background = theme.background();
    let (top_color, bottom_color) = (color(top, theme), color(bottom, theme));
    match (top_color, bottom_color) {
        (None, None) => (" ", Style::new().bg(background)),
        (Some(fg), None) => (UPPER_HALF, Style::new().fg(fg).bg(background)),
        (None, Some(fg)) => (LOWER_HALF, Style::new().fg(fg).bg(background)),
        (Some(fg), Some(bg)) if fg == bg => (FULL, Style::new().fg(fg).bg(background)),
        (Some(fg), Some(bg)) => (UPPER_HALF, Style::new().fg(fg).bg(bg)),
    }
}

/// The real color when the terminal can show it. Otherwise, the closest of the two blues.
fn color(pixel: u32, theme: &Theme) -> Option<Color> {
    if pixel == 0 {
        return None;
    }
    let (red, green, blue) = (pixel >> 16 & 0xff, pixel >> 8 & 0xff, pixel & 0xff);
    if theme.support == ColorSupport::TrueColor {
        return Some(Color::Rgb(red as u8, green as u8, blue as u8));
    }
    let brightness = (red + green + blue) / 3;
    if brightness < VISIBLE {
        return None;
    }
    let (light, dark) = theme.logo_tones();
    Some(if brightness >= LIGHT { light } else { dark })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(support: ColorSupport) -> Theme {
        Theme::new(support, true)
    }

    fn rendered(size: LogoSize, theme: &Theme) -> Buffer {
        let (width, height) = size.cells();
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        draw(size, theme, area, &mut buffer);
        buffer
    }

    fn is_blue(color: Color) -> bool {
        match color {
            Color::Rgb(red, _, blue) => red == 0 && blue == 0 || blue >= red,
            Color::Reset | Color::Blue | Color::Black => true,
            Color::Indexed(index) => [16, 67, 110].contains(&index),
            _ => false,
        }
    }

    #[test]
    fn the_logo_uses_only_blue_on_black() {
        for support in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Basic,
        ] {
            let theme = theme(support);
            for size in [LogoSize::Large, LogoSize::Medium] {
                let buffer = rendered(size, &theme);
                let mut drawn = 0;
                for cell in buffer.content() {
                    assert!(is_blue(cell.fg), "{support:?}: {:?}", cell.fg);
                    assert!(is_blue(cell.bg), "{support:?}: {:?}", cell.bg);
                    if cell.symbol() != " " {
                        drawn += 1;
                    }
                }
                assert!(drawn > 10, "{support:?} {size:?} drew only {drawn} cells");
            }
        }
    }

    #[test]
    fn the_logo_size_follows_the_terminal() {
        let color = theme(ColorSupport::TrueColor);
        assert_eq!(LogoSize::for_terminal(&color, 50), LogoSize::Large);
        assert_eq!(LogoSize::for_terminal(&color, 24), LogoSize::Medium);
        assert_eq!(LogoSize::for_terminal(&color, 12), LogoSize::Text);
    }

    #[test]
    fn limited_terminals_get_text() {
        let plain = theme(ColorSupport::None);
        assert_eq!(LogoSize::for_terminal(&plain, 50), LogoSize::Text);
        let ascii = Theme::new(ColorSupport::TrueColor, false);
        assert_eq!(LogoSize::for_terminal(&ascii, 50), LogoSize::Text);
        let buffer = rendered(LogoSize::Text, &plain);
        assert_eq!(buffer.content()[0].symbol(), "N");
    }

    #[test]
    fn drawing_outside_the_area_is_ignored() {
        let area = Rect::new(0, 0, 5, 2);
        let mut buffer = Buffer::empty(area);
        draw(
            LogoSize::Large,
            &theme(ColorSupport::TrueColor),
            area,
            &mut buffer,
        );
    }
}
