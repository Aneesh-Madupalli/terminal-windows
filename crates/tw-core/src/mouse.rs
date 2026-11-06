/// Pixel to `SendInput` absolute coordinate (0..=65535) at the centre of the pixel.
pub fn normalize_absolute(coord: u32, extent: u32) -> u16 {
    if extent == 0 {
        return 0;
    }
    let numerator = (2 * u64::from(coord) + 1) * 65536;
    let denominator = 2 * u64::from(extent);
    numerator.div_ceil(denominator).min(65535) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows maps a normalized value `n` back to pixel `floor(n * extent / 65536)`.
    #[test]
    fn every_pixel_round_trips() {
        for extent in [640u32, 800, 1024, 1280, 1366, 1920, 2560, 3840] {
            for x in 0..extent {
                let n = u64::from(normalize_absolute(x, extent));
                assert_eq!(n * u64::from(extent) / 65536, u64::from(x), "x={x} extent={extent}");
            }
        }
    }

    #[test]
    fn stays_in_range() {
        assert_eq!(normalize_absolute(0, 0), 0);
        assert!(normalize_absolute(u32::MAX / 4, 10) == 65535);
    }
}
