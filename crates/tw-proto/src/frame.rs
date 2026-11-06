//! Frame pixel codec: BGRA rows compressed with LZ4 blocks. The decoder is told the expected size,
//! so a hostile peer cannot make it allocate more than the frame header allows.

use crate::error::ProtoError;

/// Largest frame edge the protocol accepts (TDD section 9.7).
pub const MAX_EDGE: u32 = 8192;

pub fn compress(pixels: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress(pixels)
}

/// Checks the frame geometry and decompresses to exactly `bytes_per_row * height` bytes.
pub fn decompress(data: &[u8], width: u32, height: u32, bytes_per_row: u32) -> Result<Vec<u8>, ProtoError> {
    let valid = (1..=MAX_EDGE).contains(&width)
        && (1..=MAX_EDGE).contains(&height)
        && bytes_per_row >= width * 4
        && bytes_per_row <= width * 4 + 1024;
    if !valid {
        return Err(ProtoError::InvalidHeader(format!(
            "invalid frame geometry {width}x{height} stride {bytes_per_row}"
        )));
    }
    let expected = bytes_per_row as usize * height as usize;
    let pixels = lz4_flex::block::decompress(data, expected)
        .map_err(|e| ProtoError::InvalidHeader(format!("corrupt frame: {e}")))?;
    if pixels.len() != expected {
        return Err(ProtoError::InvalidHeader("frame size does not match its header".into()));
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        (0..width * height * 4).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn round_trips() {
        let pixels = gradient(64, 32);
        let packed = compress(&pixels);
        assert_eq!(decompress(&packed, 64, 32, 256).unwrap(), pixels);
    }

    #[test]
    fn flat_desktops_compress_well() {
        let pixels = vec![0x20u8; 1280 * 800 * 4];
        assert!(compress(&pixels).len() < pixels.len() / 50);
    }

    #[test]
    fn rejects_bad_geometry() {
        let packed = compress(&gradient(4, 4));
        for (w, h, stride) in
            [(0, 4, 16), (4, 0, 16), (4, 4, 15), (4, 4, 16 + 1025), (MAX_EDGE + 1, 4, 4 * (MAX_EDGE + 1))]
        {
            assert!(decompress(&packed, w, h, stride).is_err(), "{w}x{h} {stride}");
        }
    }

    #[test]
    fn rejects_a_size_that_disagrees_with_the_header() {
        let packed = compress(&gradient(4, 4));
        assert!(decompress(&packed, 4, 5, 16).is_err());
        assert!(decompress(&packed, 4, 3, 16).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(decompress(&[0xFF; 32], 4, 4, 16).is_err());
        assert!(decompress(&[], 4, 4, 16).is_err());
    }
}
