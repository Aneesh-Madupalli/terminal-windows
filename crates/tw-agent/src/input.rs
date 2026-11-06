//! Turns validated actions into input events. Pure functions, unit-tested; the Windows side only injects.

use std::time::Duration;

use tw_core::keys::{Chord, Modifier};
use tw_proto::{Action, Button, ErrorCode};

use crate::desktop::{Desktop, DesktopError, Op};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Send(Vec<Op>),
    Pause(Duration),
}

/// `SendInput` calls stay small: at most this many events per call.
const MAX_BATCH: usize = 64;
const VK_RETURN: u16 = 0x0D;
const VK_TAB: u16 = 0x09;
const VK_DELETE: u16 = 0x2E;
const DRAG_PAUSE: Duration = Duration::from_millis(8);

/// Maps a pixel to the 0..=65535 range `MOUSEEVENTF_ABSOLUTE` expects, so the pointer lands on that pixel.
pub fn normalize(pixel: u32, extent: u32) -> i32 {
    let extent = u64::from(extent.max(1));
    let scaled = ((u64::from(pixel) * 2 + 1) * 65536).div_ceil(extent * 2);
    scaled.min(65535) as i32
}

fn click_ops(button: Button, count: u8) -> Vec<Op> {
    let right = button == Button::Right;
    (0..count).flat_map(|_| [Op::Button { right, down: true }, Op::Button { right, down: false }]).collect()
}

fn type_ops(text: &str) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let virtual_key = match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                Some(VK_RETURN)
            }
            '\n' => Some(VK_RETURN),
            '\t' => Some(VK_TAB),
            _ => None,
        };
        if let Some(vk) = virtual_key {
            ops.push(Op::Key { vk, extended: false, down: true });
            ops.push(Op::Key { vk, extended: false, down: false });
            continue;
        }
        let mut units = [0u16; 2];
        for unit in character.encode_utf16(&mut units) {
            ops.push(Op::Unicode { unit: *unit, down: true });
            ops.push(Op::Unicode { unit: *unit, down: false });
        }
    }
    ops
}

fn chord_ops(chord: &Chord) -> Vec<Op> {
    let mut ops: Vec<Op> =
        chord.modifiers.iter().map(|m| Op::Key { vk: m.vk(), extended: false, down: true }).collect();
    ops.push(Op::Key { vk: chord.key.vk, extended: chord.key.extended, down: true });
    ops.push(Op::Key { vk: chord.key.vk, extended: chord.key.extended, down: false });
    ops.extend(chord.modifiers.iter().rev().map(|m| Op::Key { vk: m.vk(), extended: false, down: false }));
    ops
}

fn drag_steps_count(from: (u32, u32), to: (u32, u32)) -> u32 {
    let dx = f64::from(from.0.abs_diff(to.0));
    let dy = f64::from(from.1.abs_diff(to.1));
    ((dx.hypot(dy) / 20.0).ceil() as u32).clamp(2, 30)
}

/// The events for one request. Unsupported or status-like actions yield nothing.
pub fn plan(action: &Action) -> Result<Vec<Step>, DesktopError> {
    let steps = match action {
        Action::Click { x, y, button, count } => {
            let mut ops = vec![Op::MoveTo { x: *x, y: *y }];
            ops.extend(click_ops(*button, *count));
            vec![Step::Send(ops)]
        }
        Action::Move { x, y } => vec![Step::Send(vec![Op::MoveTo { x: *x, y: *y }])],
        Action::Drag { x, y, x2, y2 } => {
            let count = drag_steps_count((*x, *y), (*x2, *y2));
            let mut steps = vec![
                Step::Send(vec![Op::MoveTo { x: *x, y: *y }]),
                Step::Pause(DRAG_PAUSE),
                Step::Send(vec![Op::Button { right: false, down: true }]),
            ];
            for i in 1..=count {
                let lerp = |a: u32, b: u32| {
                    (i64::from(a) + (i64::from(b) - i64::from(a)) * i64::from(i) / i64::from(count)) as u32
                };
                steps.push(Step::Pause(DRAG_PAUSE));
                steps.push(Step::Send(vec![Op::MoveTo { x: lerp(*x, *x2), y: lerp(*y, *y2) }]));
            }
            steps.push(Step::Pause(DRAG_PAUSE));
            steps.push(Step::Send(vec![Op::Button { right: false, down: false }]));
            steps
        }
        Action::Scroll { at, dx, dy } => {
            let mut ops = Vec::new();
            if let Some((x, y)) = at {
                ops.push(Op::MoveTo { x: *x, y: *y });
            }
            if *dy != 0 {
                ops.push(Op::Wheel { horizontal: false, delta: *dy });
            }
            if *dx != 0 {
                // A positive protocol dx scrolls the view left, which is a negative horizontal wheel delta.
                ops.push(Op::Wheel { horizontal: true, delta: -*dx });
            }
            vec![Step::Send(ops)]
        }
        Action::Type { text } => type_ops(text).chunks(MAX_BATCH).map(|chunk| Step::Send(chunk.to_vec())).collect(),
        Action::Key { chord } => {
            let ctrl_alt = chord.modifiers.contains(&Modifier::Ctrl) && chord.modifiers.contains(&Modifier::Alt);
            if ctrl_alt && chord.key.vk == VK_DELETE {
                return Err(DesktopError::new(
                    ErrorCode::BadRequest,
                    "Ctrl+Alt+Delete cannot be sent from inside the guest; the host sends it through the hypervisor",
                ));
            }
            vec![Step::Send(chord_ops(chord))]
        }
        _ => Vec::new(),
    };
    Ok(steps)
}

/// Runs the steps on the desktop. Blocking.
pub fn execute(desktop: &dyn Desktop, steps: &[Step]) -> Result<(), DesktopError> {
    for step in steps {
        match step {
            Step::Send(ops) => desktop.send(ops)?,
            Step::Pause(duration) => std::thread::sleep(*duration),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tw_core::keys::parse_chord;

    use super::*;
    use crate::desktop::fake::FakeDesktop;

    fn ops(steps: &[Step]) -> Vec<Op> {
        steps.iter().filter_map(|s| if let Step::Send(o) = s { Some(o.clone()) } else { None }).flatten().collect()
    }

    #[test]
    fn normalization_lands_on_the_requested_pixel() {
        for extent in [800u32, 1080, 1280, 1920, 2560, 3840] {
            for pixel in [0, 1, extent / 3, extent / 2, extent - 1] {
                let n = normalize(pixel, extent) as u64;
                // Windows maps a normalized value back with floor(n * extent / 65536).
                let back = n * u64::from(extent) / 65536;
                assert_eq!(back, u64::from(pixel), "pixel {pixel} of {extent} came back as {back}");
            }
        }
        assert_eq!(normalize(5000, 1280), 65535);
    }

    #[test]
    fn click_moves_then_presses_count_times() {
        let steps = plan(&Action::Click { x: 10, y: 20, button: Button::Right, count: 2 }).unwrap();
        assert_eq!(
            ops(&steps),
            vec![
                Op::MoveTo { x: 10, y: 20 },
                Op::Button { right: true, down: true },
                Op::Button { right: true, down: false },
                Op::Button { right: true, down: true },
                Op::Button { right: true, down: false },
            ]
        );
    }

    #[test]
    fn drag_interpolates_between_the_points() {
        let steps = plan(&Action::Drag { x: 0, y: 0, x2: 200, y2: 100 }).unwrap();
        let ops = ops(&steps);
        assert_eq!(ops[0], Op::MoveTo { x: 0, y: 0 });
        assert_eq!(ops[1], Op::Button { right: false, down: true });
        assert_eq!(ops[ops.len() - 2], Op::MoveTo { x: 200, y: 100 });
        assert_eq!(ops[ops.len() - 1], Op::Button { right: false, down: false });
        let moves = ops.iter().filter(|o| matches!(o, Op::MoveTo { .. })).count();
        assert!((3..=31).contains(&moves), "{moves}");
        assert!(steps.iter().any(|s| matches!(s, Step::Pause(_))));
        // A tiny drag still has at least two intermediate moves.
        let tiny = plan(&Action::Drag { x: 5, y: 5, x2: 6, y2: 5 }).unwrap();
        assert!(ops_count_moves(&tiny) >= 3);
    }

    fn ops_count_moves(steps: &[Step]) -> usize {
        ops(steps).iter().filter(|o| matches!(o, Op::MoveTo { .. })).count()
    }

    #[test]
    fn scroll_signs_match_the_protocol() {
        let steps = plan(&Action::Scroll { at: Some((3, 4)), dx: 120, dy: 240 }).unwrap();
        assert_eq!(
            ops(&steps),
            vec![
                Op::MoveTo { x: 3, y: 4 },
                Op::Wheel { horizontal: false, delta: 240 },
                Op::Wheel { horizontal: true, delta: -120 },
            ]
        );
        assert_eq!(
            ops(&plan(&Action::Scroll { at: None, dx: 0, dy: -120 }).unwrap()),
            vec![Op::Wheel { horizontal: false, delta: -120 }]
        );
    }

    #[test]
    fn typing_uses_unicode_events_and_real_keys_for_control_characters() {
        let steps = plan(&Action::Type { text: "a\r\nb\tc".into() }).unwrap();
        assert_eq!(
            ops(&steps),
            vec![
                Op::Unicode { unit: 'a' as u16, down: true },
                Op::Unicode { unit: 'a' as u16, down: false },
                Op::Key { vk: VK_RETURN, extended: false, down: true },
                Op::Key { vk: VK_RETURN, extended: false, down: false },
                Op::Unicode { unit: 'b' as u16, down: true },
                Op::Unicode { unit: 'b' as u16, down: false },
                Op::Key { vk: VK_TAB, extended: false, down: true },
                Op::Key { vk: VK_TAB, extended: false, down: false },
                Op::Unicode { unit: 'c' as u16, down: true },
                Op::Unicode { unit: 'c' as u16, down: false },
            ]
        );
    }

    #[test]
    fn typing_sends_surrogate_pairs_as_two_units() {
        let steps = plan(&Action::Type { text: "\u{1F600}".into() }).unwrap();
        let units: Vec<u16> = ops(&steps)
            .iter()
            .filter_map(|o| if let Op::Unicode { unit, down: true } = o { Some(*unit) } else { None })
            .collect();
        assert_eq!(units, vec![0xD83D, 0xDE00]);
    }

    #[test]
    fn long_text_is_sent_in_bounded_batches() {
        let steps = plan(&Action::Type { text: "x".repeat(1000) }).unwrap();
        assert!(steps.len() > 1);
        for step in &steps {
            let Step::Send(batch) = step else { panic!("only sends") };
            assert!(batch.len() <= MAX_BATCH);
        }
        assert_eq!(ops(&steps).len(), 2000);
    }

    #[test]
    fn chords_press_modifiers_in_order_and_release_in_reverse() {
        let chord = parse_chord("ctrl+shift+esc").unwrap();
        let ops = ops(&plan(&Action::Key { chord }).unwrap());
        assert_eq!(
            ops,
            vec![
                Op::Key { vk: 0x11, extended: false, down: true },
                Op::Key { vk: 0x10, extended: false, down: true },
                Op::Key { vk: 0x1B, extended: false, down: true },
                Op::Key { vk: 0x1B, extended: false, down: false },
                Op::Key { vk: 0x10, extended: false, down: false },
                Op::Key { vk: 0x11, extended: false, down: false },
            ]
        );
        let arrow = ops_of_key("left");
        assert!(matches!(arrow[0], Op::Key { extended: true, .. }));
    }

    fn ops_of_key(key: &str) -> Vec<Op> {
        ops(&plan(&Action::Key { chord: parse_chord(key).unwrap() }).unwrap())
    }

    #[test]
    fn ctrl_alt_delete_is_refused() {
        let error = plan(&Action::Key { chord: parse_chord("ctrl+alt+del").unwrap() }).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("hypervisor"));
        assert!(plan(&Action::Key { chord: parse_chord("ctrl+del").unwrap() }).is_ok());
    }

    #[test]
    fn execute_sends_everything_and_reports_failures() {
        let desktop = FakeDesktop::new(100, 100);
        let steps = plan(&Action::Click { x: 1, y: 1, button: Button::Left, count: 1 }).unwrap();
        execute(&desktop, &steps).unwrap();
        assert_eq!(desktop.sent.lock().unwrap().len(), 3);
        *desktop.fail_send.lock().unwrap() = true;
        assert_eq!(execute(&desktop, &steps).unwrap_err().code, ErrorCode::InputFailed);
    }

    #[test]
    fn read_only_actions_plan_nothing() {
        assert!(plan(&Action::Ping).unwrap().is_empty());
        assert!(plan(&Action::Screenshot).unwrap().is_empty());
    }
}
