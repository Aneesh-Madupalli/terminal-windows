//! The Ctrl+G shortcut palette: Windows shortcuts a terminal cannot send itself.

use tw_term::chrome::{self, Span, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A key or chord sent to the guest.
    Key(&'static str),
    /// Sent through the hypervisor, because the guest cannot receive it.
    CtrlAltDel,
    Console,
    Redraw,
    Quit,
}

pub struct Item {
    pub key: &'static str,
    pub label: &'static str,
    pub action: Action,
}

const fn item(key: &'static str, label: &'static str, action: Action) -> Item {
    Item { key, label, action }
}

pub const ITEMS: &[Item] = &[
    item("d", "Ctrl+Alt+Del", Action::CtrlAltDel),
    item("o", "VM window", Action::Console),
    item("r", "Redraw", Action::Redraw),
    item("q", "Quit viewer", Action::Quit),
    item("s", "Start menu", Action::Key("win")),
    item("x", "Run dialog", Action::Key("win+r")),
    item("e", "File Explorer", Action::Key("win+e")),
    item("w", "Show desktop", Action::Key("win+d")),
    item("t", "Task Manager", Action::Key("ctrl+shift+esc")),
    item("a", "Switch app", Action::Key("alt+tab")),
    item("c", "Close window", Action::Key("alt+f4")),
    item("l", "Lock screen", Action::Key("win+l")),
    item("i", "Settings", Action::Key("win+i")),
    item("v", "Clipboard history", Action::Key("win+v")),
];

pub fn find(key: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|item| item.key == key)
}

const COLUMN_WIDTH: usize = 29;

/// A box above the status bar listing as many shortcuts as fit.
pub fn render(theme: &Theme, columns: usize, rows: usize) -> String {
    let per_row = if columns >= COLUMN_WIDTH * 2 + 4 { 2 } else { 1 };
    let inner_rows = ITEMS.len().div_ceil(per_row).min(rows.saturating_sub(3)).max(1);
    let width = (COLUMN_WIDTH * per_row + 2).min(columns);
    let top = rows.saturating_sub(inner_rows + 2).max(1);
    let title = [Span::new("Shortcuts", theme.bold())];
    let mut out = chrome::frame(1, top, width, inner_rows + 2, &title, &theme.border());
    for line in 0..inner_rows {
        let mut spans = Vec::new();
        for slot in 0..per_row {
            let Some(item) = ITEMS.get(line + slot * inner_rows) else { continue };
            spans.push(Span::new(format!(" {} ", item.key), theme.key()));
            spans.push(Span::new(format!(" {:<width$}", item.label, width = COLUMN_WIDTH - 5), theme.muted()));
        }
        out += &chrome::line(2, top + 1 + line, width.saturating_sub(2), &spans);
    }
    out
}

#[cfg(test)]
mod tests {
    use tw_core::keys::parse_chord;

    use super::*;

    #[test]
    fn every_key_is_unique_and_every_chord_is_understood_by_the_agent() {
        let mut keys: Vec<&str> = ITEMS.iter().map(|i| i.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ITEMS.len());
        for item in ITEMS {
            if let Action::Key(chord) = item.action {
                assert!(parse_chord(chord).is_ok(), "{chord}");
            }
        }
        assert_eq!(find("t").map(|i| i.action), Some(Action::Key("ctrl+shift+esc")));
        assert!(find("z").is_none());
    }

    #[test]
    fn a_wide_terminal_shows_every_shortcut_in_two_columns() {
        let theme = Theme::new(false);
        let text = render(&theme, 100, 30);
        for item in ITEMS {
            assert!(text.contains(item.label), "{}", item.label);
        }
        assert!(text.contains("Shortcuts"));
        // 14 items in two columns: 7 rows plus the frame, ending just above the status bar.
        assert!(text.starts_with("\x1b[21;1H"), "{}", text.chars().take(10).collect::<String>());
    }

    #[test]
    fn a_small_terminal_shows_the_first_shortcuts_that_fit() {
        let theme = Theme::new(false);
        let text = render(&theme, 40, 11);
        assert!(text.contains("Ctrl+Alt+Del") && text.contains("Quit viewer"));
        assert!(!text.contains("Clipboard history"), "does not fit");
    }
}
