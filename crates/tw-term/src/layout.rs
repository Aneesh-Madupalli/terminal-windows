/// Fits an image of `width x height` into a region of terminal cells, and maps cells back to image pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageLayout {
    pub column: usize,
    pub row: usize,
    pub columns: usize,
    pub rows: usize,
    pub pixel_width: f64,
    pub pixel_height: f64,
    pub cell_width: f64,
    pub cell_height: f64,
    pub width: usize,
    pub height: usize,
}

impl ImageLayout {
    /// Fits and centres the image inside the cells starting at (`origin_column`, `origin_row`), both 1-based.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: usize,
        height: usize,
        origin_column: usize,
        origin_row: usize,
        columns: usize,
        rows: usize,
        cell_width: f64,
        cell_height: f64,
    ) -> Self {
        let available_columns = columns.max(1);
        let available_rows = rows.max(1);
        let height_limited =
            available_rows as f64 * cell_height / height as f64 <= available_columns as f64 * cell_width / width as f64;
        let (pixel_width, pixel_height, used_columns, used_rows);
        if height_limited {
            used_rows = available_rows;
            pixel_height = available_rows as f64 * cell_height;
            pixel_width = pixel_height * width as f64 / height as f64;
            used_columns = ((pixel_width / cell_width).ceil() as usize).max(1);
        } else {
            used_columns = available_columns;
            pixel_width = available_columns as f64 * cell_width;
            pixel_height = pixel_width * height as f64 / width as f64;
            used_rows = ((pixel_height / cell_height).ceil() as usize).max(1);
        }
        Self {
            column: (origin_column + available_columns.saturating_sub(used_columns) / 2).max(1),
            row: (origin_row + available_rows.saturating_sub(used_rows) / 2).max(1),
            columns: used_columns,
            rows: used_rows,
            pixel_width,
            pixel_height,
            cell_width,
            cell_height,
            width,
            height,
        }
    }

    /// The image pixel under a terminal cell. With `clamp`, cells outside the image map to its nearest edge,
    /// so a drag that leaves the display keeps tracking.
    pub fn point(&self, column: usize, row: usize, clamp: bool) -> Option<(usize, usize)> {
        let x = (column as f64 - self.column as f64 + 0.5) * self.cell_width;
        let y = (row as f64 - self.row as f64 + 0.5) * self.cell_height;
        if !clamp && !(x >= 0.0 && y >= 0.0 && x < self.pixel_width && y < self.pixel_height) {
            return None;
        }
        let px = (x * self.width as f64 / self.pixel_width).floor().clamp(0.0, (self.width - 1) as f64);
        let py = (y * self.height as f64 / self.pixel_height).floor().clamp(0.0, (self.height - 1) as f64);
        Some((px as usize, py as usize))
    }
}

/// Where the desktop sits: a canvas of whole cells, with the image drawn from its top-left corner at exactly
/// `image_width x image_height` device pixels so the terminal never resamples it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPlacement {
    pub column: usize,
    pub row: usize,
    pub columns: usize,
    pub rows: usize,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub image_width: usize,
    pub image_height: usize,
    /// Opaque fill for transports without alpha.
    pub background: Option<u32>,
}

impl DesktopPlacement {
    /// `scale` converts cell units to device pixels (iTerm2 reports cells in points).
    pub fn from_layout(layout: &ImageLayout, scale: f64, background: Option<u32>) -> Self {
        let pixels = |value: f64| ((value * scale).round() as usize).max(1);
        Self {
            column: layout.column,
            row: layout.row,
            columns: layout.columns,
            rows: layout.rows,
            canvas_width: pixels(layout.columns as f64 * layout.cell_width),
            canvas_height: pixels(layout.rows as f64 * layout.cell_height),
            image_width: pixels(layout.pixel_width),
            image_height: pixels(layout.pixel_height),
            background,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_desktop_in_a_wide_terminal_is_height_limited() {
        // 1280x800 (16:10) into 100x30 cells of 10x20 px (1000x600 px).
        let layout = ImageLayout::new(1280, 800, 1, 1, 100, 30, 10.0, 20.0);
        assert_eq!(layout.rows, 30);
        assert_eq!(layout.pixel_height, 600.0);
        assert_eq!(layout.pixel_width, 960.0);
        assert_eq!(layout.columns, 96);
        assert_eq!(layout.column, 3, "centred: (100 - 96) / 2 = 2 columns of margin");
    }

    #[test]
    fn a_narrow_terminal_is_width_limited() {
        let layout = ImageLayout::new(1280, 800, 1, 1, 40, 40, 10.0, 20.0);
        assert_eq!(layout.columns, 40);
        assert_eq!(layout.pixel_width, 400.0);
        assert_eq!(layout.pixel_height, 250.0);
        assert_eq!(layout.rows, 13);
        assert!(layout.row > 1, "centred vertically");
    }

    #[test]
    fn cells_map_to_pixels_and_back_inside_the_image() {
        let layout = ImageLayout::new(1280, 800, 1, 1, 100, 30, 10.0, 20.0);
        let first = layout.point(layout.column, layout.row, false).unwrap();
        assert!(first.0 < 8 && first.1 < 20, "{first:?}");
        let last = layout.point(layout.column + layout.columns - 1, layout.row + layout.rows - 1, false).unwrap();
        assert!(last.0 > 1260 && last.1 > 780, "{last:?}");
        assert!(last.0 < 1280 && last.1 < 800);
    }

    #[test]
    fn outside_the_image_is_none_unless_clamped() {
        let layout = ImageLayout::new(1280, 800, 1, 1, 100, 30, 10.0, 20.0);
        assert_eq!(layout.point(1, 1, false), None);
        assert_eq!(layout.point(layout.column + layout.columns + 5, layout.row, false), None);
        assert_eq!(layout.point(1, 1, true).map(|p| p.0), Some(0), "left of the image clamps to its edge");
        assert_eq!(layout.point(500, 500, true), Some((1279, 799)));
    }

    #[test]
    fn placement_scales_cell_units_to_device_pixels() {
        let layout = ImageLayout::new(1280, 800, 1, 1, 100, 30, 10.0, 20.0);
        let one = DesktopPlacement::from_layout(&layout, 1.0, None);
        let two = DesktopPlacement::from_layout(&layout, 2.0, Some(0x1a1b26));
        assert_eq!((one.canvas_width, one.canvas_height), (960, 600));
        assert_eq!((two.canvas_width, two.canvas_height), (1920, 1200));
        assert_eq!(two.background, Some(0x1a1b26));
        assert!(one.image_width <= one.canvas_width && one.image_height <= one.canvas_height);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        let layout = ImageLayout::new(1280, 800, 1, 1, 0, 0, 8.0, 16.0);
        assert!(layout.columns >= 1 && layout.rows >= 1);
        assert!(layout.point(1, 1, true).is_some());
    }
}
