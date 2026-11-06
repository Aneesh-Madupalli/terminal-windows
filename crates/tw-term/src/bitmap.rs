/// An RGBA image, row-major, 8 bits per channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Bitmap {
    /// A transparent bitmap.
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, rgba: vec![0; width * height * 4] }
    }

    pub fn solid(width: usize, height: usize, rgba: [u8; 4]) -> Self {
        Self { width, height, rgba: rgba.repeat(width * height) }
    }

    /// Converts BGRA rows with a stride, as the guest agent sends them. Alpha is forced opaque.
    pub fn from_bgra(width: usize, height: usize, stride: usize, bgra: &[u8]) -> Option<Self> {
        if width == 0 || height == 0 || stride < width * 4 || bgra.len() < stride * height {
            return None;
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in bgra.chunks(stride).take(height) {
            for pixel in row[..width * 4].chunks_exact(4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        Some(Self { width, height, rgba })
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    fn set(&mut self, x: usize, y: usize, pixel: [u8; 4]) {
        let i = (y * self.width + x) * 4;
        self.rgba[i..i + 4].copy_from_slice(&pixel);
    }

    /// Area-averaged when shrinking, nearest when enlarging.
    pub fn resize(&self, width: usize, height: usize) -> Bitmap {
        let mut out = Bitmap::new(width, height);
        if width == 0 || height == 0 || self.width == 0 || self.height == 0 {
            return out;
        }
        for y in 0..height {
            let y0 = y * self.height / height;
            let y1 = ((y + 1) * self.height / height).max(y0 + 1).min(self.height);
            for x in 0..width {
                let x0 = x * self.width / width;
                let x1 = ((x + 1) * self.width / width).max(x0 + 1).min(self.width);
                let mut sum = [0u32; 4];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        for (total, value) in sum.iter_mut().zip(self.pixel(sx, sy)) {
                            *total += u32::from(value);
                        }
                    }
                }
                let count = ((y1 - y0) * (x1 - x0)) as u32;
                out.set(x, y, sum.map(|total| (total / count) as u8));
            }
        }
        out
    }

    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&self.rgba).expect("PNG data");
        writer.finish().expect("PNG end");
        out
    }
}

/// Draws `image` at the top-left of a canvas with softly rounded corners, like a display.
/// The rest of the canvas is `background` (opaque) or transparent.
pub fn compose(image: &Bitmap, canvas_width: usize, canvas_height: usize, background: Option<u32>) -> Bitmap {
    let fill = match background {
        Some(rgb) => [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255],
        None => [0, 0, 0, 0],
    };
    let mut canvas = Bitmap::solid(canvas_width, canvas_height, fill);
    let radius = (image.width.min(image.height) as f64 * 0.012).round() as usize;
    for y in 0..image.height.min(canvas_height) {
        for x in 0..image.width.min(canvas_width) {
            if outside_corner(x, y, image.width, image.height, radius) {
                continue;
            }
            canvas.set(x, y, image.pixel(x, y));
        }
    }
    canvas
}

fn outside_corner(x: usize, y: usize, width: usize, height: usize, radius: usize) -> bool {
    if radius == 0 {
        return false;
    }
    let cx = if x < radius {
        radius - 1 - x
    } else if x + radius >= width {
        x + radius - width
    } else {
        return false;
    };
    let cy = if y < radius {
        radius - 1 - y
    } else if y + radius >= height {
        y + radius - height
    } else {
        return false;
    };
    // Distance from the circle centre, in half pixels.
    (2 * cx + 1).pow(2) + (2 * cy + 1).pow(2) > (2 * radius).pow(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_becomes_rgba_and_honours_the_stride() {
        // Two pixels per row, stride 12 (4 bytes of padding), two rows.
        let bgra = [
            10, 20, 30, 0, 40, 50, 60, 0, 9, 9, 9, 9, //
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9,
        ];
        let bitmap = Bitmap::from_bgra(2, 2, 12, &bgra).unwrap();
        assert_eq!(bitmap.pixel(0, 0), [30, 20, 10, 255]);
        assert_eq!(bitmap.pixel(1, 0), [60, 50, 40, 255]);
        assert_eq!(bitmap.pixel(0, 1), [3, 2, 1, 255]);
        assert!(Bitmap::from_bgra(2, 2, 12, &bgra[..20]).is_none());
        assert!(Bitmap::from_bgra(2, 2, 7, &bgra).is_none());
        assert!(Bitmap::from_bgra(0, 2, 12, &bgra).is_none());
    }

    #[test]
    fn shrinking_averages() {
        let mut image = Bitmap::new(2, 2);
        for (x, y, v) in [(0, 0, 0), (1, 0, 100), (0, 1, 100), (1, 1, 200)] {
            image.set(x, y, [v, v, v, 255]);
        }
        let small = image.resize(1, 1);
        assert_eq!(small.pixel(0, 0), [100, 100, 100, 255]);
    }

    #[test]
    fn enlarging_repeats_pixels() {
        let image = Bitmap::solid(1, 1, [1, 2, 3, 255]).resize(3, 2);
        assert_eq!((image.width, image.height), (3, 2));
        assert!(image.rgba.chunks(4).all(|p| p == [1, 2, 3, 255]));
        assert_eq!(Bitmap::solid(4, 4, [0; 4]).resize(0, 5).rgba.len(), 0);
    }

    #[test]
    fn png_has_the_signature_and_decodes_back() {
        let image = Bitmap::solid(5, 3, [9, 8, 7, 255]);
        let bytes = image.to_png();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height), (5, 3));
        assert_eq!(&buffer[..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn compose_places_the_image_top_left_with_rounded_corners() {
        let image = Bitmap::solid(1000, 600, [255, 0, 0, 255]);
        let canvas = compose(&image, 1200, 700, None);
        assert_eq!(canvas.pixel(500, 300), [255, 0, 0, 255]);
        assert_eq!(canvas.pixel(1100, 650)[3], 0, "outside the image stays transparent");
        assert_eq!(canvas.pixel(0, 0)[3], 0, "the corner is cut");
        assert_eq!(canvas.pixel(999, 599)[3], 0);
        assert_eq!(canvas.pixel(500, 0), [255, 0, 0, 255], "edges between the corners are kept");
        let opaque = compose(&image, 1200, 700, Some(0x1a1b26));
        assert_eq!(opaque.pixel(1100, 650), [0x1a, 0x1b, 0x26, 255]);
    }

    #[test]
    fn compose_clips_an_oversized_image() {
        let image = Bitmap::solid(50, 50, [1, 1, 1, 255]);
        let canvas = compose(&image, 20, 20, None);
        assert_eq!((canvas.width, canvas.height), (20, 20));
    }
}
