//! Graphics encoders: each turns a bitmap or PNG into the bytes a terminal needs to show it.

use std::collections::BTreeMap;
use std::fmt::Write;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bitmap::Bitmap;

/// Kitty graphics protocol, PNG transmitted in 4096-byte chunks. The image sits below cell backgrounds
/// (negative z-index), so chrome and the shortcut palette stay on top. `previous` is deleted after the new
/// image is placed, so the screen never shows a gap.
pub fn kitty(png: &[u8], id: u32, previous: Option<u32>, columns: usize, rows: usize) -> String {
    let encoded = STANDARD.encode(png);
    let chunks: Vec<&str> =
        encoded.as_bytes().chunks(4096).map(|c| std::str::from_utf8(c).expect("base64 is ASCII")).collect();
    let mut out = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            let _ =
                write!(out, "\x1b_Ga=T,f=100,i={id},q=2,C=1,c={columns},r={rows},z=-1073741825,m={more};{chunk}\x1b\\");
        } else {
            let _ = write!(out, "\x1b_Gm={more};{chunk}\x1b\\");
        }
    }
    if let Some(previous) = previous {
        out += &kitty_delete(previous);
    }
    out
}

pub fn kitty_delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")
}

/// iTerm2 inline image (also understood by WezTerm).
pub fn iterm(png: &[u8], columns: usize, rows: usize) -> String {
    format!("\x1b]1337;File=inline=1;width={columns};height={rows};preserveAspectRatio=0:{}\x07", STANDARD.encode(png))
}

/// Two image pixels per terminal cell: the upper half block with the upper pixel as foreground and the lower as
/// background. The cursor is positioned at (`row`, `column`), both 1-based.
pub fn blocks(bitmap: &Bitmap, column: usize, row: usize, columns: usize, rows: usize) -> String {
    let scaled = bitmap.resize(columns, rows * 2);
    let mut out = String::new();
    for cell_row in 0..rows {
        let _ = write!(out, "\x1b[{};{}H", row + cell_row, column);
        let mut last: Option<([u8; 4], [u8; 4])> = None;
        for x in 0..columns {
            let upper = scaled.pixel(x, cell_row * 2);
            let lower = scaled.pixel(x, cell_row * 2 + 1);
            if last != Some((upper, lower)) {
                let _ = write!(
                    out,
                    "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
                    upper[0], upper[1], upper[2], lower[0], lower[1], lower[2]
                );
                last = Some((upper, lower));
            }
            out.push('\u{2580}');
        }
        out += "\x1b[0m";
    }
    out
}

/// A fixed 240-colour palette: a 6x6x6 cube plus 24 greys, so no per-frame palette search is needed.
fn palette_index(r: u8, g: u8, b: u8) -> u8 {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min <= 12 {
        let grey = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
        216 + (grey * 23 / 255) as u8
    } else {
        let level = |v: u8| (u16::from(v) * 5 + 127) / 255;
        (level(r) * 36 + level(g) * 6 + level(b)) as u8
    }
}

fn palette_color(index: u8) -> [u8; 3] {
    if index >= 216 {
        let grey = (u16::from(index - 216) * 255 / 23) as u8;
        [grey; 3]
    } else {
        let level = |v: u8| v * 51;
        [level(index / 36), level(index / 6 % 6), level(index % 6)]
    }
}

/// Sixel graphics. Pixels with less than half opacity are left transparent.
pub fn sixel(bitmap: &Bitmap) -> String {
    let (width, height) = (bitmap.width, bitmap.height);
    let mut used = [false; 256];
    let mut body = String::new();
    for band in (0..height).step_by(6) {
        let mut masks: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        for dy in 0..6.min(height - band) {
            for x in 0..width {
                let [r, g, b, a] = bitmap.pixel(x, band + dy);
                if a < 128 {
                    continue;
                }
                let index = palette_index(r, g, b);
                used[index as usize] = true;
                masks.entry(index).or_insert_with(|| vec![0; width])[x] |= 1 << dy;
            }
        }
        let count = masks.len();
        for (position, (index, row)) in masks.iter().enumerate() {
            let _ = write!(body, "#{index}");
            run_length(&mut body, row);
            body.push(if position + 1 < count { '$' } else { '-' });
        }
        if masks.is_empty() {
            body.push('-');
        }
    }
    let mut out = format!("\x1bP0;1;0q\"1;1;{width};{height}");
    for index in (0..=255u8).filter(|i| used[*i as usize]) {
        let [r, g, b] = palette_color(index);
        let percent = |v: u8| (u16::from(v) * 100 + 127) / 255;
        let _ = write!(out, "#{index};2;{};{};{}", percent(r), percent(g), percent(b));
    }
    out += &body;
    out += "\x1b\\";
    out
}

fn run_length(out: &mut String, masks: &[u8]) {
    let mut i = 0;
    while i < masks.len() {
        let value = masks[i];
        let run = masks[i..].iter().take_while(|&&m| m == value).count();
        let character = char::from(63 + value);
        if run > 3 {
            let _ = write!(out, "!{run}{character}");
        } else {
            out.extend(std::iter::repeat_n(character, run));
        }
        i += run;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_chunks_large_images_and_replaces_the_previous_one() {
        let png = vec![7u8; 10_000];
        let out = kitty(&png, 3, Some(2), 80, 24);
        assert!(out.starts_with("\x1b_Ga=T,f=100,i=3,q=2,C=1,c=80,r=24,z=-1073741825,m=1;"));
        assert_eq!(out.matches("\x1b_Gm=1;").count(), 2, "four chunks: first, two middle, one last");
        assert_eq!(out.matches("\x1b_Gm=0;").count(), 1);
        assert!(out.ends_with(&kitty_delete(2)));
        let single = kitty(b"x", 2, None, 1, 1);
        assert!(single.contains("m=0;") && !single.contains("a=d"));
    }

    #[test]
    fn kitty_payload_decodes_back() {
        let png: Vec<u8> = (0..9000u32).map(|i| (i % 256) as u8).collect();
        let out = kitty(&png, 2, None, 1, 1);
        let mut data = String::new();
        for part in out.split("\x1b\\").filter(|p| !p.is_empty()) {
            data += part.split_once(';').unwrap().1;
        }
        assert_eq!(STANDARD.decode(data).unwrap(), png);
    }

    #[test]
    fn iterm_wraps_the_png() {
        let out = iterm(b"abc", 10, 5);
        assert!(out.starts_with("\x1b]1337;File=inline=1;width=10;height=5;preserveAspectRatio=0:"));
        assert!(out.ends_with("\x07"));
        assert!(out.contains(&STANDARD.encode(b"abc")));
    }

    #[test]
    fn blocks_use_half_blocks_with_two_pixels_per_cell() {
        let mut bitmap = Bitmap::new(1, 2);
        bitmap.rgba = vec![255, 0, 0, 255, 0, 0, 255, 255];
        let out = blocks(&bitmap, 3, 4, 1, 1);
        assert_eq!(out, "\x1b[4;3H\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m\u{2580}\x1b[0m");
    }

    #[test]
    fn blocks_skip_repeated_colours() {
        let out = blocks(&Bitmap::solid(4, 2, [9, 9, 9, 255]), 1, 1, 4, 1);
        assert_eq!(out.matches("\x1b[38;2;").count(), 1);
        assert_eq!(out.matches('\u{2580}').count(), 4);
    }

    #[test]
    fn palette_covers_cube_and_greys() {
        assert_eq!(palette_index(255, 0, 0), 180);
        assert_eq!(palette_index(0, 0, 255), 5);
        assert_eq!(palette_index(0, 0, 0), 216);
        assert_eq!(palette_index(255, 255, 255), 239);
        assert_eq!(palette_color(180), [255, 0, 0]);
        assert_eq!(palette_color(239), [255, 255, 255]);
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    assert!(palette_index(r as u8, g as u8, b as u8) <= 239);
                }
            }
        }
    }

    #[test]
    fn sixel_structure() {
        let out = sixel(&Bitmap::solid(2, 6, [255, 0, 0, 255]));
        assert!(out.starts_with("\x1bP0;1;0q\"1;1;2;6#180;2;100;0;0#180"), "{out}");
        assert!(out.contains("#180~~-"), "{out}");
        assert!(out.ends_with("\x1b\\"));
    }

    #[test]
    fn sixel_run_length_encodes_long_runs() {
        let out = sixel(&Bitmap::solid(20, 6, [0, 0, 255, 255]));
        assert!(out.contains("!20~"), "{out}");
    }

    #[test]
    fn sixel_partial_bands_and_transparency() {
        let mut bitmap = Bitmap::solid(3, 7, [255, 0, 0, 255]);
        for x in 0..3 {
            bitmap.rgba[x * 4 + 3] = 0;
        }
        let out = sixel(&bitmap);
        assert_eq!(out.matches('-').count(), 2, "two bands for seven rows");
        assert!(out.contains("#180}}}"), "the first row is transparent so the mask is 0b111110: {out}");
        assert!(out.contains("#180@@@"), "the last band has a single row: {out}");
    }

    #[test]
    fn fully_transparent_input_still_terminates_bands() {
        let out = sixel(&Bitmap::new(2, 6));
        assert!(out.ends_with("-\x1b\\"));
    }
}
