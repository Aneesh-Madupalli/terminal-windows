/// Converts the guest's PNG screenshot to a JPEG, which keeps the message to the model small.
/// Returns the JPEG and the image size in pixels.
pub fn png_to_jpeg(png: &[u8], quality: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let mut reader =
        png::Decoder::new(std::io::Cursor::new(png)).read_info().map_err(|e| format!("invalid PNG: {e}"))?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("the PNG is too large")?];
    let info = reader.next_frame(&mut buffer).map_err(|e| format!("invalid PNG: {e}"))?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err("only 8-bit screenshots are supported".into());
    }
    let pixels = &buffer[..info.buffer_size()];
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => pixels.to_vec(),
        png::ColorType::Rgba => pixels.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0]]).collect(),
        png::ColorType::Indexed => return Err("indexed screenshots are not supported".into()),
    };
    let (width, height) = (u16::try_from(info.width), u16::try_from(info.height));
    let (Ok(width16), Ok(height16)) = (width, height) else {
        return Err("the screenshot is too large for JPEG".into());
    };
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, quality)
        .encode(&rgb, width16, height16, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| format!("cannot encode the screenshot: {e}"))?;
    Ok((jpeg, info.width, info.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn rgba_and_rgb_convert() {
        let rgba = png_of(4, 3, png::ColorType::Rgba, &[200u8; 4 * 3 * 4]);
        let (jpeg, w, h) = png_to_jpeg(&rgba, 85).unwrap();
        assert_eq!((w, h), (4, 3));
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "a JPEG starts with the SOI marker");
        let rgb = png_of(2, 2, png::ColorType::Rgb, &[10u8; 12]);
        assert!(png_to_jpeg(&rgb, 85).is_ok());
        let grey = png_of(2, 2, png::ColorType::Grayscale, &[10u8; 4]);
        assert!(png_to_jpeg(&grey, 85).is_ok());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(png_to_jpeg(b"not a png", 85).is_err());
        assert!(png_to_jpeg(&[], 85).is_err());
    }
}
