use std::collections::BTreeMap;

/// Removes terminal size reports, colour replies and graphics replies from the input stream,
/// keeping what they say. Everything else passes through to the input parser.
#[derive(Debug, Default)]
pub struct TerminalMetricFilter {
    pub cell_width: Option<f64>,
    pub cell_height: Option<f64>,
    /// iTerm2 reports cells in points plus a scale; other terminals report device pixels.
    pub cell_scale: Option<f64>,
    /// The terminal background as 0xRRGGBB, from an OSC 11 reply.
    pub background: Option<u32>,
    /// Kitty graphics replies keyed by image id, such as `31 -> "OK"`.
    pub graphics_replies: BTreeMap<i64, String>,
    pub window_width: Option<f64>,
    pub window_height: Option<f64>,
    /// The Primary Device Attributes reply (`ESC [ ? 61 ; 4 ; ... c`); 4 means Sixel.
    pub device_attributes: Option<Vec<u32>>,
    pending: Vec<u8>,
}

fn is_final(byte: u8) -> bool {
    (64..=126).contains(&byte)
}

impl TerminalMetricFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scale(&self) -> f64 {
        self.cell_scale.unwrap_or(1.0)
    }

    pub fn supports_sixel(&self) -> bool {
        self.device_attributes.as_ref().is_some_and(|attributes| attributes.contains(&4))
    }

    pub fn feed(&mut self, bytes: &[u8], flush: bool) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut result = Vec::new();
        while !self.pending.is_empty() {
            if self.pending[0] != 27 {
                result.push(self.pending.remove(0));
                continue;
            }
            if self.pending.len() == 1 {
                if flush {
                    result.push(self.pending.remove(0));
                }
                break;
            }
            let handled = match self.pending[1] {
                b'[' => self.csi(&mut result),
                b']' => self.osc(&mut result),
                b'_' => self.apc(),
                _ => {
                    result.push(self.pending.remove(0));
                    Some(())
                }
            };
            if handled.is_none() {
                break;
            }
            if self.pending.len() > 4096 {
                self.pending.clear();
            }
        }
        // An unterminated sequence must not make the buffer grow forever.
        if self.pending.len() > 4096 {
            self.pending.clear();
        }
        result
    }

    /// `None` means the sequence is incomplete.
    fn csi(&mut self, result: &mut Vec<u8>) -> Option<()> {
        let end = (2..self.pending.len()).find(|&i| is_final(self.pending[i]))?;
        let sequence: Vec<u8> = self.pending.drain(..=end).collect();
        let body = String::from_utf8_lossy(&sequence[2..sequence.len() - 1]).into_owned();
        match sequence[sequence.len() - 1] {
            b't' => {
                let values: Vec<f64> = body.split(';').filter_map(|v| v.parse().ok()).collect();
                if let [kind, first, second] = values[..] {
                    if first > 0.0 && second > 0.0 && first.is_finite() && second.is_finite() {
                        if kind == 6.0 {
                            self.cell_height = Some(first);
                            self.cell_width = Some(second);
                            return Some(());
                        }
                        if kind == 4.0 {
                            self.window_height = Some(first);
                            self.window_width = Some(second);
                            return Some(());
                        }
                    }
                }
            }
            b'c' if body.starts_with('?') => {
                self.device_attributes = Some(body[1..].split(';').filter_map(|v| v.parse().ok()).collect());
                return Some(());
            }
            _ => {}
        }
        result.extend(sequence);
        Some(())
    }

    fn osc(&mut self, result: &mut Vec<u8>) -> Option<()> {
        let end = (2..self.pending.len())
            .find(|&i| self.pending[i] == 7 || (self.pending[i] == b'\\' && self.pending[i - 1] == 27))?;
        let sequence: Vec<u8> = self.pending.drain(..=end).collect();
        let text = String::from_utf8_lossy(&sequence).into_owned();
        let separators = |c: char| matches!(c, ';' | '\u{7}' | '\u{1b}' | '\\');
        if let Some(position) = text.find("1337;ReportCellSize=") {
            let values: Vec<f64> = text[position + "1337;ReportCellSize=".len()..]
                .split(separators)
                .filter_map(|v| v.parse().ok())
                .collect();
            if values.len() >= 2 && (1.0..=512.0).contains(&values[0]) && (1.0..=512.0).contains(&values[1]) {
                self.cell_height = Some(values[0]);
                self.cell_width = Some(values[1]);
                if let Some(scale) = values.get(2).filter(|s| (1.0..=4.0).contains(*s)) {
                    self.cell_scale = Some(*scale);
                }
            }
        } else if let Some(position) = text.find("]11;rgb:") {
            let channels: Vec<u32> = text[position + "]11;rgb:".len()..]
                .split(['/', '\u{7}', '\u{1b}', '\\'])
                .take(3)
                .filter_map(|c| u32::from_str_radix(c.get(..2)?, 16).ok())
                .collect();
            if let [r, g, b] = channels[..] {
                self.background = Some(r << 16 | g << 8 | b);
            }
        } else {
            result.extend(sequence);
        }
        Some(())
    }

    /// Kitty graphics replies: `ESC _ G i=31;OK ESC \`.
    fn apc(&mut self) -> Option<()> {
        let end = (2..self.pending.len()).find(|&i| self.pending[i] == b'\\' && self.pending[i - 1] == 27)?;
        let body = String::from_utf8_lossy(&self.pending[2..end - 1]).into_owned();
        self.pending.drain(..=end);
        if let Some(rest) = body.strip_prefix('G') {
            if let Some((keys, message)) = rest.split_once(';') {
                let id = keys.split(',').find_map(|k| k.strip_prefix("i=")).and_then(|v| v.parse().ok());
                if let Some(id) = id {
                    self.graphics_replies.insert(id, message.to_string());
                }
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_input_passes_through() {
        let mut f = TerminalMetricFilter::new();
        assert_eq!(f.feed(b"hello", false), b"hello");
    }

    #[test]
    fn cell_and_window_size_reports_are_consumed() {
        let mut f = TerminalMetricFilter::new();
        assert!(f.feed(b"\x1b[6;20;10t\x1b[4;800;1000t", false).is_empty());
        assert_eq!((f.cell_width, f.cell_height), (Some(10.0), Some(20.0)));
        assert_eq!((f.window_width, f.window_height), (Some(1000.0), Some(800.0)));
        assert_eq!(f.scale(), 1.0);
    }

    #[test]
    fn invalid_reports_are_ignored_but_not_leaked_as_keys() {
        let mut f = TerminalMetricFilter::new();
        let out = f.feed(b"\x1b[6;0;10t", false);
        assert_eq!(f.cell_width, None);
        assert_eq!(out, b"\x1b[6;0;10t", "unrecognised sequences pass through");
    }

    #[test]
    fn iterm_cell_size_and_scale() {
        let mut f = TerminalMetricFilter::new();
        assert!(f.feed(b"\x1b]1337;ReportCellSize=18.0;9.0;2.0\x07", false).is_empty());
        assert_eq!((f.cell_height, f.cell_width, f.cell_scale), (Some(18.0), Some(9.0), Some(2.0)));
        assert!(f.feed(b"\x1b]1337;ReportCellSize=9999;9;1\x1b\\", false).is_empty());
        assert_eq!(f.cell_height, Some(18.0), "out-of-range values are ignored");
    }

    #[test]
    fn background_colour() {
        let mut f = TerminalMetricFilter::new();
        assert!(f.feed(b"\x1b]11;rgb:1a1a/1b1b/2626\x07", false).is_empty());
        assert_eq!(f.background, Some(0x1a1b26));
    }

    #[test]
    fn kitty_graphics_replies() {
        let mut f = TerminalMetricFilter::new();
        assert!(f.feed(b"\x1b_Gi=31;OK\x1b\\", false).is_empty());
        assert_eq!(f.graphics_replies.get(&31).map(String::as_str), Some("OK"));
        f.feed(b"\x1b_Gi=31;ENOENT:nope\x1b\\", false);
        assert_eq!(f.graphics_replies[&31], "ENOENT:nope");
    }

    #[test]
    fn device_attributes_detect_sixel() {
        let mut f = TerminalMetricFilter::new();
        assert!(!f.supports_sixel());
        assert!(f.feed(b"\x1b[?61;4;6;22c", false).is_empty());
        assert_eq!(f.device_attributes, Some(vec![61, 4, 6, 22]));
        assert!(f.supports_sixel());
        let mut g = TerminalMetricFilter::new();
        g.feed(b"\x1b[?62;22c", false);
        assert!(!g.supports_sixel());
    }

    #[test]
    fn replies_mixed_with_keys_and_split_across_reads() {
        let mut f = TerminalMetricFilter::new();
        assert_eq!(f.feed(b"a\x1b[6;20", false), b"a");
        assert_eq!(f.feed(b";10tb\x1b[A", false), b"b\x1b[A");
        assert_eq!(f.cell_height, Some(20.0));
    }

    #[test]
    fn a_lone_escape_waits_unless_flushed() {
        let mut f = TerminalMetricFilter::new();
        assert!(f.feed(b"\x1b", false).is_empty());
        assert_eq!(f.feed(b"", true), b"\x1b");
    }

    #[test]
    fn unterminated_garbage_is_bounded() {
        let mut f = TerminalMetricFilter::new();
        f.feed(b"\x1b]", false);
        f.feed(&vec![b'x'; 10_000], false);
        assert!(f.pending.len() <= 4096 + 10);
    }
}
