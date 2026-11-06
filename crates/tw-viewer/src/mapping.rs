//! Turns what the terminal sends into agent requests: mouse gestures, keys and text.

use std::collections::BTreeMap;

use tw_core::keys::parse_chord;
use tw_term::ImageLayout;

/// One request for the guest agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub action: &'static str,
    pub args: BTreeMap<String, String>,
}

fn request(action: &'static str, pairs: &[(&str, String)]) -> Request {
    Request { action, args: pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect() }
}

/// Where the desktop is drawn and how its pixels relate to the guest screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub layout: ImageLayout,
    /// The size of the frame that was drawn.
    pub frame: (u32, u32),
    /// The guest screen in pixels.
    pub screen: (u32, u32),
}

impl Geometry {
    /// The guest pixel under a terminal cell (1-based), or `None` outside the image unless `clamp`.
    pub fn pixel(&self, column: u32, row: u32, clamp: bool) -> Option<(u32, u32)> {
        let (x, y) = self.layout.point(column as usize, row as usize, clamp)?;
        let scale = |value: usize, frame: u32, screen: u32| {
            (((value as u64 * 2 + 1) * u64::from(screen) / (u64::from(frame.max(1)) * 2)) as u32)
                .min(screen.saturating_sub(1))
        };
        Some((scale(x, self.frame.0, self.screen.0), scale(y, self.frame.1, self.screen.1)))
    }
}

#[derive(Debug, Clone, Copy)]
struct Press {
    right: bool,
    start: (u32, u32),
    cell: (u32, u32),
}

/// Remembers a pressed button so a press and release become a click or a drag.
#[derive(Debug, Default)]
pub struct MouseMapper {
    pressed: Option<Press>,
}

/// One wheel notch.
const NOTCH: i32 = 120;

impl MouseMapper {
    pub fn event(
        &mut self,
        button: u32,
        column: u32,
        row: u32,
        released: bool,
        geometry: &Geometry,
    ) -> Option<Request> {
        if button & 64 != 0 {
            let (x, y) = geometry.pixel(column, row, false)?;
            let (dx, dy) = match button & 3 {
                0 => (0, NOTCH),
                1 => (0, -NOTCH),
                2 => (NOTCH, 0),
                _ => (-NOTCH, 0),
            };
            let mut pairs = vec![("x", x.to_string()), ("y", y.to_string())];
            if dy != 0 {
                pairs.push(("dy", dy.to_string()));
            }
            if dx != 0 {
                pairs.push(("dx", dx.to_string()));
            }
            return Some(request("scroll", &pairs));
        }
        let base = button & 3;
        if released {
            let press = self.pressed.take()?;
            let end = geometry.pixel(column, row, true)?;
            let moved = (column, row) != press.cell;
            if moved && !press.right {
                return Some(request(
                    "drag",
                    &[
                        ("x", press.start.0.to_string()),
                        ("y", press.start.1.to_string()),
                        ("x2", end.0.to_string()),
                        ("y2", end.1.to_string()),
                    ],
                ));
            }
            let mut pairs = vec![("x", press.start.0.to_string()), ("y", press.start.1.to_string())];
            if press.right {
                pairs.push(("button", "right".into()));
            }
            return Some(request("click", &pairs));
        }
        if button & 32 != 0 {
            return None; // motion while a button is held; the release decides
        }
        if base == 0 || base == 2 {
            if let Some(start) = geometry.pixel(column, row, false) {
                self.pressed = Some(Press { right: base == 2, start, cell: (column, row) });
            }
        }
        None
    }
}

/// A key or chord from the terminal, if the agent understands it.
pub fn key_request(name: &str) -> Option<Request> {
    parse_chord(name).ok()?;
    Some(request("key", &[("key", name.to_string())]))
}

pub fn text_request(text: &str) -> Request {
    request("type", &[("text", text.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1280x800 screen drawn at half size: 640x400 frame in 64x20 cells of 10x20 px.
    fn geometry() -> Geometry {
        Geometry {
            layout: ImageLayout::new(640, 400, 1, 1, 64, 20, 10.0, 20.0),
            frame: (640, 400),
            screen: (1280, 800),
        }
    }

    fn arg<'a>(r: &'a Request, key: &str) -> &'a str {
        r.args.get(key).map_or("", String::as_str)
    }

    #[test]
    fn cells_map_to_guest_pixels() {
        let g = geometry();
        let (x, y) = g.pixel(g.layout.column as u32, g.layout.row as u32, false).unwrap();
        assert!(x < 20 && y < 40, "({x}, {y})");
        let (x, y) = g
            .pixel((g.layout.column + g.layout.columns - 1) as u32, (g.layout.row + g.layout.rows - 1) as u32, false)
            .unwrap();
        assert!(x > 1240 && x < 1280 && y > 760 && y < 800, "({x}, {y})");
        assert_eq!(g.pixel(1000, 1000, false), None);
        assert_eq!(g.pixel(1000, 1000, true), Some((1279, 799)));
    }

    #[test]
    fn a_press_and_release_in_one_cell_is_a_click() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        assert_eq!(mapper.event(0, 10, 5, false, &g), None);
        let click = mapper.event(0, 10, 5, true, &g).unwrap();
        assert_eq!(click.action, "click");
        assert!(!click.args.contains_key("button"));
        assert!(arg(&click, "x").parse::<u32>().unwrap() < 1280);
        assert_eq!(mapper.event(0, 10, 5, true, &g), None, "the press was consumed");
    }

    #[test]
    fn a_right_button_click_is_a_right_click_even_if_it_moves() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        mapper.event(2, 10, 5, false, &g);
        let click = mapper.event(2, 14, 7, true, &g).unwrap();
        assert_eq!((click.action, arg(&click, "button")), ("click", "right"));
    }

    #[test]
    fn moving_with_the_left_button_down_is_a_drag() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        mapper.event(0, 10, 5, false, &g);
        assert_eq!(mapper.event(32, 12, 6, false, &g), None, "motion alone sends nothing");
        let drag = mapper.event(0, 30, 10, true, &g).unwrap();
        assert_eq!(drag.action, "drag");
        for key in ["x", "y", "x2", "y2"] {
            assert!(drag.args.contains_key(key), "{key}");
        }
        assert!(arg(&drag, "x2").parse::<u32>().unwrap() > arg(&drag, "x").parse::<u32>().unwrap());
    }

    #[test]
    fn a_drag_released_outside_the_image_still_ends_on_its_edge() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        mapper.event(0, 10, 5, false, &g);
        let drag = mapper.event(0, 500, 500, true, &g).unwrap();
        assert_eq!((arg(&drag, "x2"), arg(&drag, "y2")), ("1279", "799"));
    }

    #[test]
    fn presses_outside_the_image_are_ignored() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        assert_eq!(mapper.event(0, 500, 500, false, &g), None);
        assert_eq!(mapper.event(0, 500, 500, true, &g), None);
    }

    #[test]
    fn the_middle_button_does_nothing() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        mapper.event(1, 10, 5, false, &g);
        assert_eq!(mapper.event(1, 10, 5, true, &g), None);
    }

    #[test]
    fn the_wheel_scrolls_in_the_protocols_signs() {
        let g = geometry();
        let mut mapper = MouseMapper::default();
        let up = mapper.event(64, 10, 5, false, &g).unwrap();
        assert_eq!((up.action, arg(&up, "dy")), ("scroll", "120"));
        assert!(!up.args.contains_key("dx"));
        assert_eq!(arg(&mapper.event(65, 10, 5, false, &g).unwrap(), "dy"), "-120");
        assert_eq!(arg(&mapper.event(66, 10, 5, false, &g).unwrap(), "dx"), "120");
        assert_eq!(arg(&mapper.event(67, 10, 5, false, &g).unwrap(), "dx"), "-120");
        assert_eq!(mapper.event(64, 500, 500, false, &g), None, "outside the image");
    }

    #[test]
    fn keys_are_validated_before_they_are_sent() {
        assert_eq!(key_request("ctrl+c").unwrap().args["key"], "ctrl+c");
        assert!(key_request("f5").is_some());
        assert!(key_request("shift+tab").is_some());
        assert!(key_request("not-a-key").is_none());
        assert_eq!(text_request("h\u{e9}llo").args["text"], "h\u{e9}llo");
    }
}
