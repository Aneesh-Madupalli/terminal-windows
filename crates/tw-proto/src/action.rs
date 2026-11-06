use std::collections::{BTreeMap, BTreeSet};

use tw_core::keys::{Chord, parse_chord};

use crate::error::ProtoError;
use crate::message::MAX_PAYLOAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

/// A validated request, ready to execute. Ranges follow TDD section 8.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Ping,
    Status,
    Screenshot,
    Subscribe { max_width: u32, max_height: u32, fps: u32 },
    Unsubscribe,
    Click { x: u32, y: u32, button: Button, count: u8 },
    Move { x: u32, y: u32 },
    Drag { x: u32, y: u32, x2: u32, y2: u32 },
    Scroll { at: Option<(u32, u32)>, dx: i32, dy: i32 },
    Type { text: String },
    Key { chord: Chord },
    Update { sha256: String, build: u32, size: u64 },
    Logs { lines: u32 },
}

const MAX_TYPE_BYTES: usize = 32_768;

fn bad(message: impl Into<String>) -> ProtoError {
    ProtoError::BadRequest(message.into())
}

/// Reads arguments by name and rejects any that were not consumed.
struct Reader<'a> {
    args: &'a BTreeMap<String, String>,
    used: BTreeSet<&'static str>,
    display: Option<(u32, u32)>,
}

impl<'a> Reader<'a> {
    fn raw(&mut self, key: &'static str) -> Option<&'a str> {
        self.used.insert(key);
        self.args.get(key).map(String::as_str)
    }

    fn number<T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display>(
        &mut self,
        key: &'static str,
        min: T,
        max: T,
    ) -> Result<Option<T>, ProtoError> {
        let Some(raw) = self.raw(key) else { return Ok(None) };
        match raw.parse::<T>() {
            Ok(value) if value >= min && value <= max => Ok(Some(value)),
            _ => Err(bad(format!("Invalid {key}; expected {min}..={max}"))),
        }
    }

    fn required<T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display>(
        &mut self,
        key: &'static str,
        min: T,
        max: T,
    ) -> Result<T, ProtoError> {
        self.number(key, min, max)?.ok_or_else(|| bad(format!("Missing {key}")))
    }

    fn coordinate(&mut self, key: &'static str, extent: Option<u32>) -> Result<Option<u32>, ProtoError> {
        let max = extent.map_or(u32::MAX, |e| e.saturating_sub(1));
        self.number(key, 0u32, max)
    }

    fn point(&mut self, x: &'static str, y: &'static str) -> Result<(u32, u32), ProtoError> {
        let (w, h) = self.display.map_or((None, None), |(w, h)| (Some(w), Some(h)));
        let px = self.coordinate(x, w)?.ok_or_else(|| bad(format!("Missing {x}")))?;
        let py = self.coordinate(y, h)?.ok_or_else(|| bad(format!("Missing {y}")))?;
        Ok((px, py))
    }

    fn finish(self) -> Result<(), ProtoError> {
        match self.args.keys().find(|key| !self.used.contains(key.as_str())) {
            Some(key) => Err(bad(format!("Unknown argument: {key}"))),
            None => Ok(()),
        }
    }
}

/// Validates `action` and its string arguments. `display` is the guest screen in pixels when known
/// (the agent always knows it; the host may not, and then only checks that coordinates are numbers).
pub fn parse_action(
    action: &str,
    args: &BTreeMap<String, String>,
    display: Option<(u32, u32)>,
) -> Result<Action, ProtoError> {
    let mut r = Reader { args, used: BTreeSet::new(), display };
    let parsed = match action {
        "ping" => Action::Ping,
        "status" => Action::Status,
        "unsubscribe" => Action::Unsubscribe,
        "screenshot" => {
            if let Some(format) = r.raw("format") {
                if format != "png" {
                    return Err(bad("format must be png"));
                }
            }
            Action::Screenshot
        }
        "subscribe" => Action::Subscribe {
            max_width: r.required("maxWidth", 16u32, 7680)?,
            max_height: r.required("maxHeight", 16u32, 4320)?,
            fps: r.number("fps", 1u32, 30)?.unwrap_or(30),
        },
        "click" => {
            let (x, y) = r.point("x", "y")?;
            let button = match r.raw("button") {
                None | Some("left") => Button::Left,
                Some("right") => Button::Right,
                Some(_) => return Err(bad("button must be left or right")),
            };
            let mut count = r.number("count", 1u8, 3)?.unwrap_or(1);
            match r.raw("double") {
                None | Some("false") => {}
                Some("true") => count = 2,
                Some(_) => return Err(bad("double must be true or false")),
            }
            Action::Click { x, y, button, count }
        }
        "move" => {
            let (x, y) = r.point("x", "y")?;
            Action::Move { x, y }
        }
        "drag" => {
            let (x, y) = r.point("x", "y")?;
            let (x2, y2) = r.point("x2", "y2")?;
            Action::Drag { x, y, x2, y2 }
        }
        "scroll" => {
            let (w, h) = display.map_or((None, None), |(w, h)| (Some(w), Some(h)));
            let x = r.coordinate("x", w)?;
            let y = r.coordinate("y", h)?;
            let at = match (x, y) {
                (Some(x), Some(y)) => Some((x, y)),
                (None, None) => None,
                _ => return Err(bad("Provide both x and y, or neither")),
            };
            let dy = r.number("dy", -1000i32, 1000)?.unwrap_or(0);
            let dx = r.number("dx", -1000i32, 1000)?.unwrap_or(0);
            if dx == 0 && dy == 0 {
                return Err(bad("Provide a non-zero dy or dx"));
            }
            Action::Scroll { at, dx, dy }
        }
        "type" => {
            let text = r.raw("text").ok_or_else(|| bad("Provide text (maximum 32 KiB)"))?;
            if text.is_empty() || text.len() > MAX_TYPE_BYTES {
                return Err(bad("Provide text (maximum 32 KiB)"));
            }
            Action::Type { text: text.to_string() }
        }
        "key" => {
            let key = r.raw("key").ok_or_else(|| bad("Missing key"))?;
            let chord = parse_chord(key).map_err(|e| bad(e.to_string()))?;
            Action::Key { chord }
        }
        "update" => {
            let sha256 = r.raw("sha256").ok_or_else(|| bad("Missing sha256"))?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad("sha256 must be 64 hex digits"));
            }
            let sha256 = sha256.to_ascii_lowercase();
            let build = r.required("build", 1u32, u32::MAX)?;
            let size = r.required("size", 1u64, MAX_PAYLOAD as u64)?;
            Action::Update { sha256, build, size }
        }
        "logs" => Action::Logs { lines: r.number("lines", 1u32, 2000)?.unwrap_or(200) },
        other => return Err(bad(format!("Unknown action: {other}"))),
    };
    r.finish()?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Option<(u32, u32)> = Some((1280, 800));

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    fn parse(action: &str, pairs: &[(&str, &str)]) -> Result<Action, ProtoError> {
        parse_action(action, &args(pairs), DISPLAY)
    }

    #[test]
    fn simple_actions() {
        assert_eq!(parse("ping", &[]).unwrap(), Action::Ping);
        assert_eq!(parse("status", &[]).unwrap(), Action::Status);
        assert_eq!(parse("screenshot", &[("format", "png")]).unwrap(), Action::Screenshot);
        assert!(parse("screenshot", &[("format", "jpg")]).is_err());
        assert!(parse("ping", &[("x", "1")]).is_err(), "unknown arguments are rejected");
        assert!(parse("bogus", &[]).is_err());
    }

    #[test]
    fn click_variants() {
        assert_eq!(
            parse("click", &[("x", "640"), ("y", "400")]).unwrap(),
            Action::Click { x: 640, y: 400, button: Button::Left, count: 1 }
        );
        assert_eq!(
            parse("click", &[("x", "1"), ("y", "2"), ("button", "right"), ("count", "3")]).unwrap(),
            Action::Click { x: 1, y: 2, button: Button::Right, count: 3 }
        );
        assert_eq!(
            parse("click", &[("x", "1"), ("y", "2"), ("double", "true")]).unwrap(),
            Action::Click { x: 1, y: 2, button: Button::Left, count: 2 }
        );
        assert!(parse("click", &[("x", "1"), ("y", "2"), ("count", "4")]).is_err());
        assert!(parse("click", &[("x", "1"), ("y", "2"), ("count", "0")]).is_err());
        assert!(parse("click", &[("x", "1"), ("y", "2"), ("button", "middle")]).is_err());
        assert!(parse("click", &[("x", "1")]).is_err());
    }

    #[test]
    fn coordinates_are_bounds_checked_against_the_display() {
        assert!(parse("move", &[("x", "1279"), ("y", "799")]).is_ok());
        assert!(parse("move", &[("x", "1280"), ("y", "0")]).is_err());
        assert!(parse("move", &[("x", "0"), ("y", "800")]).is_err());
        assert!(parse("move", &[("x", "-1"), ("y", "0")]).is_err());
        assert!(parse("move", &[("x", "abc"), ("y", "0")]).is_err());
        // The host may not know the display; only numbers are checked then.
        assert!(parse_action("move", &args(&[("x", "5000"), ("y", "0")]), None).is_ok());
    }

    #[test]
    fn drag_needs_both_points() {
        assert!(parse("drag", &[("x", "1"), ("y", "2"), ("x2", "3"), ("y2", "4")]).is_ok());
        assert!(parse("drag", &[("x", "1"), ("y", "2"), ("x2", "3")]).is_err());
    }

    #[test]
    fn scroll_rules() {
        assert_eq!(parse("scroll", &[("dy", "-300")]).unwrap(), Action::Scroll { at: None, dx: 0, dy: -300 });
        assert_eq!(
            parse("scroll", &[("x", "10"), ("y", "20"), ("dx", "5")]).unwrap(),
            Action::Scroll { at: Some((10, 20)), dx: 5, dy: 0 }
        );
        assert!(parse("scroll", &[]).is_err(), "needs a non-zero delta");
        assert!(parse("scroll", &[("dy", "0")]).is_err());
        assert!(parse("scroll", &[("dy", "1001")]).is_err());
        assert!(parse("scroll", &[("dy", "-1001")]).is_err());
        assert!(parse("scroll", &[("x", "10"), ("dy", "1")]).is_err(), "x without y");
    }

    #[test]
    fn type_limits() {
        assert_eq!(parse("type", &[("text", "h\u{e9}llo")]).unwrap(), Action::Type { text: "h\u{e9}llo".into() });
        assert!(parse("type", &[]).is_err());
        assert!(parse("type", &[("text", "")]).is_err());
        let max = "a".repeat(MAX_TYPE_BYTES);
        assert!(parse("type", &[("text", &max)]).is_ok());
        assert!(parse("type", &[("text", &(max + "a"))]).is_err());
    }

    #[test]
    fn key_chords_are_parsed() {
        let Action::Key { chord } = parse("key", &[("key", "win+r")]).unwrap() else { panic!() };
        assert_eq!(chord.key.vk, 0x52);
        assert!(parse("key", &[("key", "bogus")]).is_err());
        assert!(parse("key", &[]).is_err());
    }

    #[test]
    fn subscribe_ranges_and_defaults() {
        assert_eq!(
            parse("subscribe", &[("maxWidth", "1280"), ("maxHeight", "800")]).unwrap(),
            Action::Subscribe { max_width: 1280, max_height: 800, fps: 30 }
        );
        assert!(parse("subscribe", &[("maxWidth", "15"), ("maxHeight", "800")]).is_err());
        assert!(parse("subscribe", &[("maxWidth", "7681"), ("maxHeight", "800")]).is_err());
        assert!(parse("subscribe", &[("maxWidth", "100"), ("maxHeight", "100"), ("fps", "31")]).is_err());
        assert!(parse("subscribe", &[("maxWidth", "100")]).is_err());
    }

    #[test]
    fn update_and_logs() {
        let sha = "A".repeat(64);
        assert_eq!(
            parse("update", &[("sha256", &sha), ("build", "2"), ("size", "1000")]).unwrap(),
            Action::Update { sha256: "a".repeat(64), build: 2, size: 1000 }
        );
        assert!(parse("update", &[("sha256", "short"), ("build", "2"), ("size", "1000")]).is_err());
        assert!(parse("update", &[("sha256", &sha), ("build", "2"), ("size", "0")]).is_err());
        assert_eq!(parse("logs", &[]).unwrap(), Action::Logs { lines: 200 });
        assert!(parse("logs", &[("lines", "2001")]).is_err());
    }
}
