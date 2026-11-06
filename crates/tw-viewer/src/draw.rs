//! Draws a frame with whichever graphics protocol the terminal supports.

use tw_term::chrome::{self, Span, Theme};
use tw_term::{Bitmap, ImageLayout, Protocol, render};

/// Kitty images are replaced rather than redrawn, so each needs an id and the old one is deleted.
#[derive(Debug, Default)]
pub struct KittyIds {
    next: u32,
    previous: Option<u32>,
}

impl KittyIds {
    fn advance(&mut self) -> (u32, Option<u32>) {
        self.next = self.next % 1_000_000 + 1;
        let previous = self.previous.replace(self.next);
        (self.next, previous)
    }

    pub fn clear(&mut self) -> String {
        self.previous.take().map(render::kitty_delete).unwrap_or_default()
    }
}

/// The bytes that draw `bitmap` inside `layout`, positioned absolutely.
pub fn frame(protocol: Protocol, bitmap: &Bitmap, layout: &ImageLayout, ids: &mut KittyIds) -> String {
    let goto = format!("\x1b[{};{}H", layout.row, layout.column);
    match protocol {
        Protocol::Kitty => {
            let (id, previous) = ids.advance();
            format!("{goto}{}", render::kitty(&bitmap.to_png(), id, previous, layout.columns, layout.rows))
        }
        Protocol::Iterm => format!("{goto}{}", render::iterm(&bitmap.to_png(), layout.columns, layout.rows)),
        Protocol::Sixel => {
            let (width, height) = (layout.pixel_width.round() as usize, layout.pixel_height.round() as usize);
            let fitted = if bitmap.width.abs_diff(width) > 1 || bitmap.height.abs_diff(height) > 1 {
                bitmap.resize(width.max(1), height.max(1))
            } else {
                bitmap.clone()
            };
            format!("{goto}{}", render::sixel(&fitted))
        }
        Protocol::Blocks => render::blocks(bitmap, layout.column, layout.row, layout.columns, layout.rows),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Lost,
}

/// What the bottom line shows.
pub struct Bar<'a> {
    pub theme: &'a Theme,
    pub columns: usize,
    pub row: usize,
    pub link: Link,
    pub name: &'a str,
    pub protocol: Protocol,
    pub notice: &'a str,
    pub menu: bool,
}

/// The bottom line: link state, machine, the active protocol and the shortcuts.
pub fn status_bar(bar: &Bar) -> String {
    let theme = bar.theme;
    let (dot, colour) = match bar.link {
        Link::Live => ("\u{25cf} live", theme.green()),
        Link::Connecting => ("\u{25cb} connecting", theme.yellow()),
        Link::Lost => ("\u{25cb} reconnecting", theme.red()),
    };
    let mut spans = vec![Span::new(format!(" {dot} "), colour), Span::new(format!(" {} ", bar.name), theme.badge())];
    if bar.menu {
        for (key, label) in [("d", "Ctrl+Alt+Del"), ("o", "Hyper-V console"), ("r", "redraw"), ("q", "quit")] {
            spans.push(Span::new(format!(" {key} "), theme.key()));
            spans.push(Span::new(format!(" {label} "), theme.muted()));
        }
        spans.push(Span::new(" Esc closes ".to_string(), theme.faint()));
    } else {
        spans.push(Span::new(format!(" {} ", bar.protocol.name()), theme.faint()));
        if !bar.notice.is_empty() {
            spans.push(Span::new(format!(" {} ", bar.notice), theme.yellow()));
        }
        spans.push(Span::new("  Ctrl+G menu  Ctrl+] quit".to_string(), theme.muted()));
    }
    chrome::line(1, bar.row, bar.columns, &spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> ImageLayout {
        ImageLayout::new(64, 32, 1, 1, 16, 8, 8.0, 8.0)
    }

    fn bitmap() -> Bitmap {
        Bitmap::solid(64, 32, [10, 20, 30, 255])
    }

    #[test]
    fn kitty_frames_replace_the_previous_image() {
        let mut ids = KittyIds::default();
        let first = frame(Protocol::Kitty, &bitmap(), &layout(), &mut ids);
        assert!(first.starts_with("\x1b[1;1H\x1b_G") && first.contains("i=1,") && !first.contains("a=d"));
        let second = frame(Protocol::Kitty, &bitmap(), &layout(), &mut ids);
        assert!(second.contains("i=2,") && second.contains("a=d,d=I,i=1"), "{second}");
        assert!(ids.clear().contains("i=2"));
        assert_eq!(ids.clear(), "");
    }

    #[test]
    fn iterm_sixel_and_blocks_are_positioned() {
        let mut ids = KittyIds::default();
        let iterm = frame(Protocol::Iterm, &bitmap(), &layout(), &mut ids);
        assert!(iterm.starts_with("\x1b[1;1H\x1b]1337;File=inline=1;width=16;height=8"));
        let sixel = frame(Protocol::Sixel, &bitmap(), &layout(), &mut ids);
        assert!(sixel.starts_with("\x1b[1;1H\x1bP"), "{}", &sixel[..20.min(sixel.len())]);
        let blocks = frame(Protocol::Blocks, &bitmap(), &layout(), &mut ids);
        assert!(blocks.starts_with("\x1b[1;1H") && blocks.contains('\u{2580}'));
    }

    fn bar<'a>(theme: &'a Theme, link: Link, notice: &'a str, menu: bool) -> Bar<'a> {
        Bar { theme, columns: 100, row: 24, link, name: "default", protocol: Protocol::Sixel, notice, menu }
    }

    #[test]
    fn the_status_bar_shows_the_link_and_shortcuts() {
        let theme = Theme::new(false);
        let live = status_bar(&bar(&theme, Link::Live, "", false));
        assert!(
            live.contains("live") && live.contains("default") && live.contains("sixel") && live.contains("Ctrl+G"),
            "{live}"
        );
        assert!(live.starts_with("\x1b[24;1H"));
        assert!(status_bar(&bar(&theme, Link::Lost, "", false)).contains("reconnecting"));
        assert!(status_bar(&bar(&theme, Link::Live, "The guest is locked", false)).contains("locked"));
        let menu = status_bar(&bar(&theme, Link::Live, "", true));
        assert!(menu.contains("Ctrl+Alt+Del") && menu.contains("Hyper-V console") && menu.contains("quit"), "{menu}");
    }
}
