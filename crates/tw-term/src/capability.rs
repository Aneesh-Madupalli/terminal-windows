use std::str::FromStr;

/// How the desktop is drawn in the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Iterm,
    Sixel,
    Blocks,
}

impl FromStr for Protocol {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "kitty" => Ok(Self::Kitty),
            "iterm" => Ok(Self::Iterm),
            "sixel" => Ok(Self::Sixel),
            "blocks" => Ok(Self::Blocks),
            _ => Err("--protocol must be kitty, iterm, sixel or blocks".into()),
        }
    }
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Iterm => "iterm",
            Self::Sixel => "sixel",
            Self::Blocks => "blocks",
        }
    }

    /// The highest frame rate worth asking the guest for: Sixel costs far more to encode.
    pub fn target_fps(self) -> u32 {
        match self {
            Self::Sixel => 10,
            Self::Blocks => 15,
            Self::Kitty | Self::Iterm => 30,
        }
    }
}

/// What the terminal answered to the probe sent at start-up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probe {
    /// The Kitty graphics query was acknowledged.
    pub kitty: bool,
    /// The Primary Device Attributes reply listed Sixel.
    pub sixel: bool,
}

/// The queries to send at start-up: Kitty graphics, device attributes, and the cell and window sizes.
pub const QUERIES: &str =
    "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[16t\x1b[14t\x1b]1337;ReportCellSize\x07\x1b]11;?\x07";

/// Picks the best protocol: a verified Kitty reply first, then known terminals, then Sixel, then half blocks.
pub fn detect(env: &dyn Fn(&str) -> Option<String>, probe: Probe) -> Protocol {
    let term_program = env("TERM_PROGRAM").unwrap_or_default();
    let term = env("TERM").unwrap_or_default();
    if probe.kitty {
        return Protocol::Kitty;
    }
    if term_program == "iTerm.app" {
        return Protocol::Iterm;
    }
    if env("KITTY_WINDOW_ID").is_some() || term.contains("kitty") || term_program == "ghostty" {
        return Protocol::Kitty;
    }
    if term_program == "WezTerm" || env("WEZTERM_EXECUTABLE").is_some() {
        return Protocol::Iterm;
    }
    if probe.sixel || env("WT_SESSION").is_some() {
        return Protocol::Sixel;
    }
    Protocol::Blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string())
    }

    #[test]
    fn parses_overrides() {
        assert_eq!("kitty".parse(), Ok(Protocol::Kitty));
        assert_eq!("blocks".parse(), Ok(Protocol::Blocks));
        assert!("png".parse::<Protocol>().is_err());
        for p in [Protocol::Kitty, Protocol::Iterm, Protocol::Sixel, Protocol::Blocks] {
            assert_eq!(p.name().parse(), Ok(p));
        }
    }

    #[test]
    fn a_kitty_reply_wins() {
        assert_eq!(detect(&env(&[("WT_SESSION", "x")]), Probe { kitty: true, sixel: true }), Protocol::Kitty);
    }

    #[test]
    fn known_terminals() {
        assert_eq!(detect(&env(&[("TERM_PROGRAM", "iTerm.app")]), Probe::default()), Protocol::Iterm);
        assert_eq!(detect(&env(&[("KITTY_WINDOW_ID", "1")]), Probe::default()), Protocol::Kitty);
        assert_eq!(detect(&env(&[("TERM_PROGRAM", "ghostty")]), Probe::default()), Protocol::Kitty);
        assert_eq!(detect(&env(&[("TERM", "xterm-kitty")]), Probe::default()), Protocol::Kitty);
        assert_eq!(detect(&env(&[("TERM_PROGRAM", "WezTerm")]), Probe::default()), Protocol::Iterm);
        assert_eq!(detect(&env(&[("WEZTERM_EXECUTABLE", "C:\\wezterm.exe")]), Probe::default()), Protocol::Iterm);
    }

    #[test]
    fn windows_terminal_and_sixel_terminals_use_sixel() {
        assert_eq!(detect(&env(&[("WT_SESSION", "guid")]), Probe::default()), Protocol::Sixel);
        assert_eq!(detect(&env(&[]), Probe { kitty: false, sixel: true }), Protocol::Sixel);
    }

    #[test]
    fn everything_else_falls_back_to_blocks() {
        assert_eq!(detect(&env(&[]), Probe::default()), Protocol::Blocks);
        assert_eq!(detect(&env(&[("TERM", "xterm-256color")]), Probe::default()), Protocol::Blocks);
    }

    #[test]
    fn sixel_is_rate_limited() {
        assert!(Protocol::Sixel.target_fps() < Protocol::Kitty.target_fps());
    }

    #[test]
    fn the_probe_asks_for_everything_the_filter_understands() {
        for needle in ["i=31", "\x1b[c", "\x1b[16t", "\x1b[14t", "ReportCellSize", "]11;?"] {
            assert!(QUERIES.contains(needle), "{needle:?}");
        }
    }
}
