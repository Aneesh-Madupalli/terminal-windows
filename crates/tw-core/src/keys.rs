use crate::error::{Error, Result};

/// A Windows virtual-key code and whether `KEYEVENTF_EXTENDEDKEY` is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub vk: u16,
    pub extended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Win,
}

impl Modifier {
    pub const fn vk(self) -> u16 {
        match self {
            Modifier::Ctrl => 0x11,
            Modifier::Alt => 0x12,
            Modifier::Shift => 0x10,
            Modifier::Win => 0x5B,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ctrl" | "control" => Some(Modifier::Ctrl),
            "alt" | "option" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            "win" | "super" | "meta" | "cmd" => Some(Modifier::Win),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: KeySpec,
}

const fn plain(vk: u16) -> Option<KeySpec> {
    Some(KeySpec { vk, extended: false })
}

const fn extended(vk: u16) -> Option<KeySpec> {
    Some(KeySpec { vk, extended: true })
}

/// Looks up a lower-case key name. Modifier names resolve to the modifier's own key (a lone `win`).
pub fn lookup_key(name: &str) -> Option<KeySpec> {
    if let Some(modifier) = Modifier::parse(name) {
        return plain(modifier.vk());
    }
    let bytes = name.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        return match c {
            b'a'..=b'z' => plain(u16::from(c - b'a') + 0x41),
            b'0'..=b'9' => plain(u16::from(c)),
            b'-' => plain(0xBD),
            b'=' => plain(0xBB),
            b'[' => plain(0xDB),
            b']' => plain(0xDD),
            b'\\' => plain(0xDC),
            b';' => plain(0xBA),
            b'\'' => plain(0xDE),
            b',' => plain(0xBC),
            b'.' => plain(0xBE),
            b'/' => plain(0xBF),
            b'`' => plain(0xC0),
            _ => None,
        };
    }
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return if (1..=12).contains(&number) { plain(0x70 + number - 1) } else { None };
    }
    match name {
        "return" | "enter" => plain(0x0D),
        "tab" => plain(0x09),
        "space" => plain(0x20),
        "backspace" => plain(0x08),
        "escape" | "esc" => plain(0x1B),
        "delete" | "del" => extended(0x2E),
        "insert" => extended(0x2D),
        "home" => extended(0x24),
        "end" => extended(0x23),
        "pageup" => extended(0x21),
        "pagedown" => extended(0x22),
        "left" => extended(0x25),
        "up" => extended(0x26),
        "right" => extended(0x27),
        "down" => extended(0x28),
        _ => None,
    }
}

/// Parses `ctrl+shift+esc`, `win+r`, `return` or a lone `win`.
pub fn parse_chord(input: &str) -> Result<Chord> {
    let lower = input.trim().to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let unknown = || Error::Usage(format!("Unknown key: {input}. Example: win+r, return, ctrl+a"));
    if parts.iter().any(|p| p.is_empty()) {
        return Err(unknown());
    }
    let (key_name, modifier_names) = parts.split_last().expect("split always yields one part");
    let key = lookup_key(key_name).ok_or_else(unknown)?;
    let mut modifiers = Vec::new();
    for name in modifier_names {
        let modifier = Modifier::parse(name).ok_or_else(|| Error::Usage(format!("Unknown modifier: {name}")))?;
        if !modifiers.contains(&modifier) {
            modifiers.push(modifier);
        }
    }
    Ok(Chord { modifiers, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys() {
        assert_eq!(lookup_key("a").unwrap().vk, 0x41);
        assert_eq!(lookup_key("z").unwrap().vk, 0x5A);
        assert_eq!(lookup_key("0").unwrap().vk, 0x30);
        assert_eq!(lookup_key("9").unwrap().vk, 0x39);
        assert_eq!(lookup_key("f1").unwrap().vk, 0x70);
        assert_eq!(lookup_key("f12").unwrap().vk, 0x7B);
        assert!(lookup_key("f13").is_none());
        assert!(lookup_key("f0").is_none());
        assert_eq!(lookup_key("f").unwrap().vk, 0x46, "a bare f is the letter");
    }

    #[test]
    fn extended_keys_are_flagged() {
        for name in ["delete", "insert", "home", "end", "pageup", "pagedown", "up", "down", "left", "right"] {
            assert!(lookup_key(name).unwrap().extended, "{name}");
        }
        for name in ["return", "tab", "space", "backspace", "escape", "a", "f5"] {
            assert!(!lookup_key(name).unwrap().extended, "{name}");
        }
    }

    #[test]
    fn aliases() {
        assert_eq!(lookup_key("enter"), lookup_key("return"));
        assert_eq!(lookup_key("esc"), lookup_key("escape"));
        assert_eq!(lookup_key("del"), lookup_key("delete"));
        assert_eq!(Modifier::parse("cmd"), Some(Modifier::Win));
        assert_eq!(Modifier::parse("super"), Some(Modifier::Win));
        assert_eq!(Modifier::parse("option"), Some(Modifier::Alt));
    }

    #[test]
    fn chords() {
        let c = parse_chord("Ctrl+Shift+Esc").unwrap();
        assert_eq!(c.modifiers, [Modifier::Ctrl, Modifier::Shift]);
        assert_eq!(c.key.vk, 0x1B);

        let c = parse_chord("win+r").unwrap();
        assert_eq!(c.modifiers, [Modifier::Win]);
        assert_eq!(c.key.vk, 0x52);

        let lone = parse_chord("win").unwrap();
        assert!(lone.modifiers.is_empty());
        assert_eq!(lone.key.vk, 0x5B);

        assert_eq!(parse_chord("ctrl+ctrl+a").unwrap().modifiers, [Modifier::Ctrl]);
    }

    #[test]
    fn rejects_bad_chords() {
        for bad in ["", "+", "ctrl+", "+a", "ctrl++a", "bogus", "ctrl+bogus", "bogus+a", "f13"] {
            assert!(parse_chord(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_key_name_in_the_table_resolves() {
        let names = [
            "return",
            "enter",
            "tab",
            "space",
            "backspace",
            "delete",
            "escape",
            "esc",
            "home",
            "end",
            "pageup",
            "pagedown",
            "insert",
            "up",
            "down",
            "left",
            "right",
            "-",
            "=",
            "[",
            "]",
            "\\",
            ";",
            "'",
            ",",
            ".",
            "/",
            "`",
        ];
        for name in names {
            assert!(lookup_key(name).is_some(), "{name}");
        }
    }
}
