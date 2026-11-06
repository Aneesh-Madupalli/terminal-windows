//! Downscaling and PNG encoding of captured bitmaps.

use crate::desktop::Bitmap;

/// The size that fits in `max` without upscaling, keeping the aspect ratio, with even dimensions.
pub fn fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    let ratio = (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height)).min(1.0);
    if ratio >= 1.0 {
        return (width, height);
    }
    let even = |v: f64| (((v.floor() as u32) / 2) * 2).max(2);
    (even(f64::from(width) * ratio), even(f64::from(height) * ratio))
}

/// Box-filter downscale, which averages every source pixel under a destination pixel.
pub fn downscale(source: &Bitmap, width: u32, height: u32) -> Bitmap {
    if width == source.width && height == source.height {
        return Bitmap::new(width, height, (0..height).flat_map(|y| source.row(y).iter().copied()).collect());
    }
    let ranges = |extent: u32, target: u32| -> Vec<(usize, usize)> {
        (0..target)
            .map(|i| {
                let start = (u64::from(i) * u64::from(extent) / u64::from(target)) as usize;
                let end = (u64::from(i + 1) * u64::from(extent) / u64::from(target)) as usize;
                (start, end.max(start + 1).min(extent as usize))
            })
            .collect()
    };
    let columns = ranges(source.width, width);
    let rows = ranges(source.height, height);
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for &(y0, y1) in &rows {
        for &(x0, x1) in &columns {
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &source.data[y * source.stride..];
                for pixel in row[x0 * 4..x1 * 4].chunks_exact(4) {
                    for (total, value) in sum.iter_mut().zip(pixel) {
                        *total += u32::from(*value);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            data.extend(sum.iter().map(|total| ((total + count / 2) / count) as u8));
        }
    }
    Bitmap::new(width, height, data)
}

/// A fast change detector over the pixel bytes. It need not be cryptographic.
pub fn fingerprint(bitmap: &Bitmap) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for y in 0..bitmap.height {
        let mut chunks = bitmap.row(y).chunks_exact(8);
        for chunk in &mut chunks {
            let value = u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
            hash = (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23);
        }
        for byte in chunks.remainder() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// PNG of the bitmap. Screen capture leaves the alpha byte undefined, so it is dropped.
pub fn to_png(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity(bitmap.width as usize * bitmap.height as usize * 3);
    for y in 0..bitmap.height {
        for pixel in bitmap.row(y).chunks_exact(4) {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, bitmap.width, bitmap.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Bitmap {
        Bitmap::new(width, height, pixel.repeat((width * height) as usize))
    }

    #[test]
    fn fit_never_upscales_and_keeps_the_aspect_ratio() {
        assert_eq!(fit(1280, 800, 2000, 2000), (1280, 800));
        assert_eq!(fit(1280, 800, 640, 640), (640, 400));
        assert_eq!(fit(1920, 1080, 800, 800), (800, 450));
        assert_eq!(fit(1000, 1000, 333, 500), (332, 332));
        assert_eq!(fit(1280, 800, 16, 16), (16, 10));
        let (w, h) = fit(1281, 801, 500, 500);
        assert!(w % 2 == 0 && h % 2 == 0 && w <= 500 && h <= 500);
    }

    #[test]
    fn downscaling_a_flat_image_keeps_its_colour() {
        let small = downscale(&solid(100, 60, [10, 20, 30, 255]), 50, 30);
        assert_eq!((small.width, small.height), (50, 30));
        assert!(small.data.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn downscaling_averages_neighbouring_pixels() {
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[0, 0, 0, 255, 200, 100, 50, 255]);
        }
        let source = Bitmap::new(2, 2, data);
        let one = downscale(&source, 1, 1);
        assert_eq!(one.data, vec![100, 50, 25, 255]);
    }

    #[test]
    fn same_size_copies_and_odd_ratios_do_not_overrun() {
        let source = solid(7, 5, [1, 2, 3, 4]);
        assert_eq!(downscale(&source, 7, 5), source);
        let tiny = downscale(&source, 3, 2);
        assert_eq!((tiny.width, tiny.height, tiny.data.len()), (3, 2, 24));
        let big = downscale(&solid(1281, 801, [9, 9, 9, 9]), 640, 400);
        assert_eq!(big.data.len(), 640 * 400 * 4);
    }

    #[test]
    fn fingerprints_detect_single_pixel_changes() {
        let a = solid(33, 9, [5, 5, 5, 255]);
        let mut b = a.clone();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let last = b.data.len() - 3;
        b.data[last] ^= 1;
        assert_ne!(fingerprint(&a), fingerprint(&b));
        let mut c = a.clone();
        c.data[0] ^= 1;
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }

    #[test]
    fn png_has_the_right_size_and_colours() {
        let bitmap = solid(4, 2, [30, 20, 10, 0]);
        let bytes = to_png(&bitmap).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut pixels = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (4, 2));
        assert_eq!(&pixels[..3], &[10, 20, 30], "BGR becomes RGB");
    }
}
