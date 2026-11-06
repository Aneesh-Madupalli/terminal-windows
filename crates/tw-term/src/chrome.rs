/// A run of text with one style. Widths count characters, so keep to single-width glyphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: String,
}

impl Span {
    pub fn new(text: impl Into<String>, style: impl Into<String>) -> Self {
        Self { text: text.into(), style: style.into() }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, "")
    }
}

pub trait Spans {
    fn width(&self) -> usize;
    fn truncated(&self, limit: usize) -> Vec<Span>;
    fn rendered(&self) -> String;
}

impl Spans for [Span] {
    fn width(&self) -> usize {
        self.iter().map(|s| s.text.chars().count()).sum()
    }

    fn truncated(&self, limit: usize) -> Vec<Span> {
        let mut result = Vec::new();
        let mut remaining = limit;
        for span in self {
            if remaining == 0 {
                break;
            }
            let length = span.text.chars().count();
            if length <= remaining {
                result.push(span.clone());
                remaining -= length;
            } else {
                let mut text: String = span.text.chars().take(remaining.saturating_sub(1)).collect();
                text.push('\u{2026}');
                result.push(Span::new(text, span.style.clone()));
                remaining = 0;
            }
        }
        result
    }

    fn rendered(&self) -> String {
        self.iter()
            .map(|s| if s.style.is_empty() { s.text.clone() } else { format!("{}{}\x1b[0m", s.style, s.text) })
            .collect()
    }
}

/// Tokyo Night-flavoured palette. With colour off (`NO_COLOR`) every style is empty.
#[derive(Debug, Clone)]
pub struct Theme {
    color: bool,
}

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const SPINNER: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}",
    "\u{280f}",
];

impl Theme {
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    /// Colour unless `NO_COLOR` is set to a non-empty value (https://no-color.org).
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()))
    }

    pub fn fg(&self, hex: u32) -> String {
        if self.color {
            format!("\x1b[38;2;{};{};{}m", hex >> 16 & 255, hex >> 8 & 255, hex & 255)
        } else {
            String::new()
        }
    }

    pub fn bg(&self, hex: u32) -> String {
        if self.color {
            format!("\x1b[48;2;{};{};{}m", hex >> 16 & 255, hex >> 8 & 255, hex & 255)
        } else {
            String::new()
        }
    }

    pub fn bold(&self) -> String {
        BOLD.to_string()
    }

    pub fn text(&self) -> String {
        self.fg(0xc0caf5)
    }
    pub fn muted(&self) -> String {
        self.fg(0x737aa2)
    }
    pub fn faint(&self) -> String {
        self.fg(0x545c7e)
    }
    pub fn border(&self) -> String {
        self.fg(0x3b4261)
    }
    pub fn accent(&self) -> String {
        self.fg(0x7aa2f7)
    }
    pub fn green(&self) -> String {
        self.fg(0x9ece6a)
    }
    pub fn yellow(&self) -> String {
        self.fg(0xe0af68)
    }
    pub fn red(&self) -> String {
        self.fg(0xf7768e)
    }
    pub fn badge(&self) -> String {
        format!("{BOLD}{}{}", self.fg(0x1a1b26), self.bg(0x7aa2f7))
    }
    pub fn key(&self) -> String {
        format!("{BOLD}{}{}", self.fg(0x7dcfff), self.bg(0x24283b))
    }
}

fn goto(row: usize, column: usize) -> String {
    format!("\x1b[{row};{column}H")
}

/// A rounded box with an optional title in its top edge. The interior is not cleared.
pub fn frame(column: usize, row: usize, width: usize, height: usize, title: &[Span], color: &str) -> String {
    if width < 2 || height < 2 {
        return String::new();
    }
    let paint = |text: String| [Span::new(text, color)].rendered();
    let label: Vec<Span> = if title.is_empty() {
        Vec::new()
    } else {
        let mut parts = vec![Span::new("\u{2500} ", color)];
        parts.extend(title.truncated(width.saturating_sub(6)));
        parts.push(Span::new(" ", color));
        parts
    };
    let mut out = goto(row, column);
    out += &paint("\u{256d}".into());
    out += &label.rendered();
    out += &paint(format!("{}\u{256e}", "\u{2500}".repeat((width - 2).saturating_sub(label.width()))));
    for offset in 1..height - 1 {
        out += &goto(row + offset, column);
        out += &paint("\u{2502}".into());
        out += &goto(row + offset, column + width - 1);
        out += &paint("\u{2502}".into());
    }
    out += &goto(row + height - 1, column);
    out += &paint(format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(width - 2)));
    out
}

/// Writes `spans` at a position, padded with spaces to `width` so stale text is erased.
pub fn line(column: usize, row: usize, width: usize, spans: &[Span]) -> String {
    let fitted = spans.truncated(width);
    format!("{}{}{}", goto(row, column), fitted.rendered(), " ".repeat(width.saturating_sub(fitted.width())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_count_characters() {
        assert_eq!([Span::plain("ab"), Span::plain("\u{25c6}")].width(), 3);
    }

    #[test]
    fn truncation_adds_an_ellipsis() {
        let spans = [Span::plain("hello"), Span::plain(" world")];
        let cut = spans.truncated(8);
        assert_eq!(cut.width(), 8);
        assert!(cut.last().unwrap().text.ends_with('\u{2026}'));
        assert_eq!(spans.truncated(50), spans);
        assert!(spans.truncated(0).is_empty());
    }

    #[test]
    fn styles_wrap_and_reset() {
        assert_eq!([Span::new("x", "\x1b[1m")].rendered(), "\x1b[1mx\x1b[0m");
        assert_eq!([Span::plain("x")].rendered(), "x");
    }

    #[test]
    fn no_color_removes_styles() {
        let off = Theme::new(false);
        assert_eq!(off.accent(), "");
        assert_eq!(off.badge(), "\x1b[1m");
        assert_eq!(Theme::new(true).fg(0x7aa2f7), "\x1b[38;2;122;162;247m");
    }

    #[test]
    fn frames_have_corners_and_are_sized() {
        let out = frame(2, 3, 6, 4, &[], "");
        assert!(out.starts_with("\x1b[3;2H\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}"));
        assert!(out.contains("\x1b[6;2H\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}"));
        assert!(out.contains("\x1b[4;7H\u{2502}"));
        assert_eq!(frame(1, 1, 1, 5, &[], ""), "");
    }

    #[test]
    fn titled_frames_keep_their_width() {
        let out = frame(1, 1, 20, 3, &[Span::plain("Title")], "");
        assert!(out.contains("\u{2500} Title "));
    }

    #[test]
    fn lines_pad_to_width() {
        assert_eq!(line(1, 1, 6, &[Span::plain("ab")]), "\x1b[1;1Hab    ");
        assert_eq!(line(1, 1, 3, &[Span::plain("abcdef")]), "\x1b[1;1Hab\u{2026}");
    }
}
