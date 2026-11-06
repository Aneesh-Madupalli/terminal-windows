/// What the terminal sent, after the escape sequences are decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Text(String),
    /// A key or chord name the guest agent understands, such as `up`, `ctrl+a` or `shift+f5`.
    Key(String),
    Mouse {
        button: u32,
        column: u32,
        row: u32,
        released: bool,
    },
    Quit,
}

/// Turns the raw bytes of a VT-input terminal into events. Partial sequences are kept until more arrive.
#[derive(Debug, Default)]
pub struct TerminalInput {
    bytes: Vec<u8>,
}

const CTRL_RIGHT_BRACKET: u8 = 29;
const ESC: u8 = 27;

fn arrow_or_nav(final_byte: u8) -> Option<&'static str> {
    Some(match final_byte {
        b'A' => "up",
        b'B' => "down",
        b'C' => "right",
        b'D' => "left",
        b'H' => "home",
        b'F' => "end",
        _ => return None,
    })
}

fn tilde_key(code: u32) -> Option<&'static str> {
    Some(match code {
        1 | 7 => "home",
        2 => "insert",
        3 => "delete",
        4 | 8 => "end",
        5 => "pageup",
        6 => "pagedown",
        11 => "f1",
        12 => "f2",
        13 => "f3",
        14 => "f4",
        15 => "f5",
        17 => "f6",
        18 => "f7",
        19 => "f8",
        20 => "f9",
        21 => "f10",
        23 => "f11",
        24 => "f12",
        _ => return None,
    })
}

/// `2` is shift, `3` alt, `5` ctrl and so on: the value is `1 + bit flags`.
fn with_modifiers(key: &str, modifier: u32) -> String {
    let bits = modifier.saturating_sub(1);
    let mut out = String::new();
    for (bit, name) in [(4, "ctrl"), (2, "alt"), (1, "shift")] {
        if bits & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(key);
    out
}

/// Names a CSI sequence such as `ESC [ 1 ; 5 A` (`ctrl+up`) or `ESC [ 15 ~` (`f5`).
fn csi_key(parameters: &str, final_byte: u8) -> Option<String> {
    let numbers: Vec<u32> = parameters.split(';').filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    let modifier = numbers.get(1).copied().unwrap_or(1);
    let base = match final_byte {
        b'~' => tilde_key(*numbers.first()?)?,
        b'Z' => return Some("shift+tab".into()),
        b'P' => "f1",
        b'Q' => "f2",
        b'R' => "f3",
        b'S' => "f4",
        other => arrow_or_nav(other)?,
    };
    Some(with_modifiers(base, modifier))
}

impl TerminalInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes not yet turned into events, for tests and diagnostics.
    pub fn pending(&self) -> usize {
        self.bytes.len()
    }

    /// `flush_escape` turns a lone ESC into an `escape` key, for when no more bytes follow.
    pub fn feed(&mut self, input: &[u8], flush_escape: bool) -> Vec<TerminalEvent> {
        self.bytes.extend_from_slice(input);
        let mut events = Vec::new();
        while !self.bytes.is_empty() {
            let first = self.bytes[0];
            if first == CTRL_RIGHT_BRACKET {
                self.bytes.remove(0);
                events.push(TerminalEvent::Quit);
            } else if first == ESC {
                if !self.escape(flush_escape, &mut events) {
                    break;
                }
            } else if first < 32 || first == 127 {
                self.bytes.remove(0);
                match first {
                    13 | 10 => events.push(TerminalEvent::Key("return".into())),
                    9 => events.push(TerminalEvent::Key("tab".into())),
                    8 | 127 => events.push(TerminalEvent::Key("backspace".into())),
                    1..=26 => events.push(TerminalEvent::Key(format!("ctrl+{}", (first + 96) as char))),
                    _ => {}
                }
            } else if !self.text(&mut events) {
                break;
            }
        }
        events
    }

    /// Handles a sequence starting with ESC. Returns false when more bytes are needed.
    fn escape(&mut self, flush: bool, events: &mut Vec<TerminalEvent>) -> bool {
        if self.bytes.len() == 1 {
            if flush {
                self.bytes.remove(0);
                events.push(TerminalEvent::Key("escape".into()));
            }
            return false;
        }
        match self.bytes[1] {
            b'[' => {
                let Some(end) = (2..self.bytes.len()).find(|&i| (64..=126).contains(&self.bytes[i])) else {
                    if self.bytes.len() > 128 {
                        self.bytes.clear();
                    }
                    return false;
                };
                let sequence: Vec<u8> = self.bytes.drain(..=end).collect();
                let body = String::from_utf8_lossy(&sequence[2..sequence.len() - 1]).into_owned();
                let final_byte = sequence[sequence.len() - 1];
                if let Some(mouse) = body.strip_prefix('<').filter(|_| final_byte == b'M' || final_byte == b'm') {
                    let parts: Vec<u32> = mouse.split(';').filter_map(|p| p.parse().ok()).collect();
                    if let [button, column, row] = parts[..] {
                        if column > 0 && row > 0 {
                            events.push(TerminalEvent::Mouse { button, column, row, released: final_byte == b'm' });
                        }
                    }
                } else if let Some(key) = csi_key(&body, final_byte) {
                    events.push(TerminalEvent::Key(key));
                }
            }
            b'O' => {
                // SS3: ESC O P is F1, ESC O A is up (application cursor mode).
                if self.bytes.len() < 3 {
                    return false;
                }
                let key = csi_key("", self.bytes[2]);
                self.bytes.drain(..3);
                events.extend(key.map(TerminalEvent::Key));
            }
            _ => {
                self.bytes.remove(0);
                events.push(TerminalEvent::Key("escape".into()));
            }
        }
        true
    }

    /// Decodes one UTF-8 character. Returns false when it is incomplete.
    fn text(&mut self, events: &mut Vec<TerminalEvent>) -> bool {
        let first = self.bytes[0];
        let length = match first {
            0..=0x7F => 1,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => {
                self.bytes.remove(0);
                return true;
            }
        };
        if self.bytes.len() < length {
            return false;
        }
        match std::str::from_utf8(&self.bytes[..length]) {
            Ok(text) => {
                events.push(TerminalEvent::Text(text.to_string()));
                self.bytes.drain(..length);
            }
            Err(_) => {
                self.bytes.remove(0);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(bytes: &[u8]) -> Vec<TerminalEvent> {
        TerminalInput::new().feed(bytes, true)
    }

    fn key(name: &str) -> TerminalEvent {
        TerminalEvent::Key(name.into())
    }

    fn text(value: &str) -> TerminalEvent {
        TerminalEvent::Text(value.into())
    }

    #[test]
    fn plain_text_and_utf8() {
        assert_eq!(feed(b"hi"), [text("h"), text("i")]);
        assert_eq!(feed("caf\u{e9} \u{65e5}\u{1f600}".as_bytes()).len(), 7);
    }

    #[test]
    fn utf8_split_across_reads() {
        let mut input = TerminalInput::new();
        let bytes = "\u{65e5}".as_bytes();
        assert!(input.feed(&bytes[..1], false).is_empty());
        assert!(input.feed(&bytes[1..2], false).is_empty());
        assert_eq!(input.feed(&bytes[2..], false), [text("\u{65e5}")]);
    }

    #[test]
    fn control_keys() {
        assert_eq!(feed(&[13]), [key("return")]);
        assert_eq!(feed(&[10]), [key("return")]);
        assert_eq!(feed(&[9]), [key("tab")]);
        assert_eq!(feed(&[127]), [key("backspace")]);
        assert_eq!(feed(&[8]), [key("backspace")]);
        assert_eq!(feed(&[1]), [key("ctrl+a")]);
        assert_eq!(feed(&[26]), [key("ctrl+z")]);
        assert_eq!(feed(&[29]), [TerminalEvent::Quit]);
    }

    #[test]
    fn arrows_and_navigation() {
        assert_eq!(feed(b"\x1b[A\x1b[B\x1b[C\x1b[D"), [key("up"), key("down"), key("right"), key("left")]);
        assert_eq!(
            feed(b"\x1b[H\x1b[F\x1b[3~\x1b[5~\x1b[6~\x1b[Z"),
            [key("home"), key("end"), key("delete"), key("pageup"), key("pagedown"), key("shift+tab")]
        );
    }

    #[test]
    fn modified_keys_and_function_keys() {
        assert_eq!(feed(b"\x1b[1;5A"), [key("ctrl+up")]);
        assert_eq!(feed(b"\x1b[1;2D"), [key("shift+left")]);
        assert_eq!(feed(b"\x1b[1;7C"), [key("ctrl+alt+right")]);
        assert_eq!(feed(b"\x1b[3;5~"), [key("ctrl+delete")]);
        assert_eq!(feed(b"\x1b[15~\x1b[24~"), [key("f5"), key("f12")]);
        assert_eq!(feed(b"\x1b[15;2~"), [key("shift+f5")]);
        assert_eq!(feed(b"\x1bOP\x1bOS\x1bOA"), [key("f1"), key("f4"), key("up")]);
        assert_eq!(feed(b"\x1b[1;2P"), [key("shift+f1")]);
    }

    #[test]
    fn every_produced_key_name_is_a_valid_chord() {
        let sequences: [&[u8]; 8] =
            [b"\x1b[A", b"\x1b[1;5B", b"\x1b[3~", b"\x1b[15;6~", b"\x1bOP", b"\x1b[Z", b"\x1b[2;3~", b"\x1b[1;8H"];
        for sequence in sequences {
            for event in feed(sequence) {
                let TerminalEvent::Key(name) = event else { panic!("{sequence:?}") };
                assert!(tw_core::keys::parse_chord(&name).is_ok(), "{name}");
            }
        }
    }

    #[test]
    fn sgr_mouse() {
        assert_eq!(feed(b"\x1b[<0;10;5M"), [TerminalEvent::Mouse { button: 0, column: 10, row: 5, released: false }]);
        assert_eq!(feed(b"\x1b[<0;10;5m"), [TerminalEvent::Mouse { button: 0, column: 10, row: 5, released: true }]);
        assert_eq!(feed(b"\x1b[<64;1;1M"), [TerminalEvent::Mouse { button: 64, column: 1, row: 1, released: false }]);
        assert!(feed(b"\x1b[<0;0;5M").is_empty(), "zero coordinates are invalid");
        assert!(feed(b"\x1b[<0;5M").is_empty(), "missing a coordinate");
    }

    #[test]
    fn a_split_escape_sequence_waits_for_the_rest() {
        let mut input = TerminalInput::new();
        assert!(input.feed(b"\x1b[<0;1", false).is_empty());
        assert!(input.pending() > 0);
        assert_eq!(input.feed(b"0;5M", false).len(), 1);
        assert_eq!(input.pending(), 0);
    }

    #[test]
    fn a_lone_escape_is_flushed_only_on_request() {
        let mut input = TerminalInput::new();
        assert!(input.feed(b"\x1b", false).is_empty());
        assert_eq!(input.feed(b"", true), [key("escape")]);
        assert_eq!(feed(b"\x1bx"), [key("escape"), text("x")]);
    }

    #[test]
    fn garbage_never_hangs_or_grows_without_bound() {
        let mut input = TerminalInput::new();
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let chunk: Vec<u8> = (0..16)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            input.feed(&chunk, false);
            assert!(input.pending() <= 160, "{}", input.pending());
        }
        input.feed(b"\x1b[", false);
        input.feed(&[b'0'; 300], false);
        assert!(input.pending() <= 160);
    }
}
